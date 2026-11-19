import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart';

const int auth = 1, authSuccess = 2, authFailure = 3, screenInfo = 4;
const int frame = 5, ping = 6, pong = 7, busy = 8, disconnectMessage = 9;
const int snapshotList = 10, snapshotListReply = 11, snapshotGet = 12;
const int snapshotFrame = 13, snapshotError = 14;
const int authWebRtc = 15, webRtcOffer = 16, webRtcAnswer = 17;
const int maxPayload = 16 * 1024 * 1024;

enum TransportMode { tcp, webRtc }

class RemoteConnection extends ChangeNotifier {
  Socket? _socket;
  RTCPeerConnection? _peer;
  RTCDataChannel? _dataChannel;
  Timer? _webRtcRecoveryTimer;
  bool _webRtcChannelOpen = false;
  TransportMode transport = TransportMode.tcp;
  int? _assemblyId, _assemblyWidth, _assemblyHeight, _assemblyLength;
  int _assemblyOffset = 0;
  Uint8List? _assembly;
  Uint8List _pending = Uint8List(0);
  Uint8List? jpeg;
  Uint8List? snapshotJpeg;
  List<int> snapshotIds = [];
  String? snapshotStatus;
  int width = 0, height = 0, fps = 0;
  int _frames = 0;
  DateTime _fpsSince = DateTime.now();
  String status = 'Disconnected';
  bool connected = false;
  bool _disposed = false;

  Future<void> connect(String host, int port, String password,
      {TransportMode transport = TransportMode.tcp}) async {
    disconnect();
    this.transport = transport;
    status = 'Connecting…';
    notifyListeners();
    try {
      final socket = await Socket.connect(
        host,
        port,
        timeout: const Duration(seconds: 5),
      );
      _socket = socket;
      socket.setOption(SocketOption.tcpNoDelay, true);
      socket.listen(
        _receive,
        onError: (Object error) {
          debugPrint('Viewer TCP error: $error');
          _fail('Connection error: $error');
        },
        onDone: () {
          debugPrint('Viewer TCP connection closed');
          if (_socket == socket) _fail('Disconnected');
        },
        cancelOnError: true,
      );
      _send(transport == TransportMode.webRtc ? authWebRtc : auth,
          utf8.encode(password));
    } catch (error) {
      _fail('Cannot connect: $error');
    }
  }

  void _send(int type, List<int> payload) {
    final header = ByteData(6)
      ..setUint8(0, 1)
      ..setUint8(1, type)
      ..setUint32(2, payload.length);
    _socket?.add([...header.buffer.asUint8List(), ...payload]);
  }

  void loadSnapshots() {
    if (!connected) return;
    snapshotStatus = 'Loading screenshots…';
    notifyListeners();
    _send(snapshotList, const []);
  }

  void loadSnapshot(int id) {
    if (!connected) return;
    snapshotJpeg = null;
    snapshotStatus = 'Loading screenshot…';
    notifyListeners();
    final bytes = ByteData(8)..setUint64(0, id);
    _send(snapshotGet, bytes.buffer.asUint8List());
  }

  Future<void> _startWebRtc() async {
    try {
      final peer = await createPeerConnection({'iceServers': <dynamic>[]});
      if (_socket == null) {
        await peer.close();
        return;
      }
      _peer = peer;
      peer.onIceConnectionState = (state) {
        debugPrint('WebRTC ICE state: $state');
        if (state == RTCIceConnectionState.RTCIceConnectionStateFailed &&
            connected) {
          _fail('WebRTC ICE connection failed');
        } else if (state ==
                RTCIceConnectionState.RTCIceConnectionStateDisconnected &&
            connected) {
          _scheduleWebRtcRecovery('WebRTC connection interrupted');
        } else if ((state ==
                    RTCIceConnectionState.RTCIceConnectionStateConnected ||
                state ==
                    RTCIceConnectionState.RTCIceConnectionStateCompleted) &&
            _webRtcChannelOpen) {
          _webRtcRecoveryTimer?.cancel();
          _webRtcRecoveryTimer = null;
        }
      };
      peer.onConnectionState = (state) {
        debugPrint('WebRTC peer state: $state');
        if (state == RTCPeerConnectionState.RTCPeerConnectionStateFailed &&
            connected) {
          _fail('WebRTC peer connection failed');
        } else if (state ==
                RTCPeerConnectionState.RTCPeerConnectionStateDisconnected &&
            connected) {
          _scheduleWebRtcRecovery('WebRTC peer connection interrupted');
        } else if (state ==
                RTCPeerConnectionState.RTCPeerConnectionStateClosed &&
            connected) {
          _fail('WebRTC peer connection closed');
        }
      };
      final channel = await peer.createDataChannel(
        'rv-frame-v1',
        RTCDataChannelInit()..ordered = true,
      );
      _dataChannel = channel;
      channel.onDataChannelState = (state) {
        if (state == RTCDataChannelState.RTCDataChannelOpen) {
          _webRtcChannelOpen = true;
          _webRtcRecoveryTimer?.cancel();
          _webRtcRecoveryTimer = null;
          status = 'Connected';
          if (!_disposed) notifyListeners();
        } else if (state == RTCDataChannelState.RTCDataChannelClosed &&
            connected) {
          // ICE can briefly drop and recover on a LAN. Don't tear down the TCP
          // signaling socket on the first channel-close notification; give the
          // peer state callbacks time to report whether the path recovered.
          _webRtcChannelOpen = false;
          _scheduleWebRtcRecovery('WebRTC connection closed');
        }
      };
      channel.onMessage = (message) {
        if (message.isBinary) {
          _receiveWebRtcFrame(message.binary);
        }
      };

      final gathered = Completer<void>();
      peer.onIceGatheringState = (state) {
        if (state == RTCIceGatheringState.RTCIceGatheringStateComplete &&
            !gathered.isCompleted) {
          gathered.complete();
        }
      };
      final offer = await peer.createOffer();
      await peer.setLocalDescription(offer);
      if (peer.iceGatheringState !=
          RTCIceGatheringState.RTCIceGatheringStateComplete) {
        await gathered.future.timeout(const Duration(seconds: 15));
      }
      final local = await peer.getLocalDescription();
      if (local == null || _socket == null) {
        throw StateError('Could not create WebRTC offer');
      }
      _send(webRtcOffer, utf8.encode(jsonEncode(local.toMap())));
      status = 'Connecting over WebRTC…';
      if (!_disposed) notifyListeners();
    } catch (error) {
      if (_socket != null) _fail('WebRTC setup failed: $error');
    }
  }

  void _scheduleWebRtcRecovery(String message) {
    status = 'WebRTC reconnecting…';
    if (!_disposed) notifyListeners();
    _webRtcRecoveryTimer?.cancel();
    _webRtcRecoveryTimer = Timer(const Duration(seconds: 8), () {
      _webRtcRecoveryTimer = null;
      if (connected) _fail(message);
    });
  }

  Future<void> _acceptWebRtcAnswer(Uint8List payload) async {
    try {
      final answer = jsonDecode(utf8.decode(payload)) as Map<String, dynamic>;
      await _peer?.setRemoteDescription(RTCSessionDescription(
        answer['sdp'] as String?,
        answer['type'] as String?,
      ));
    } catch (error) {
      _fail('Invalid WebRTC answer: $error');
    }
  }

  void _receiveWebRtcFrame(Uint8List packet) {
    if (packet.length < 28 ||
        packet[0] != 82 ||
        packet[1] != 86 ||
        packet[2] != 70 ||
        packet[3] != 49) {
      _fail('Invalid WebRTC frame');
      return;
    }
    final info = ByteData.sublistView(packet);
    final id = info.getUint64(4);
    final w = info.getUint32(12), h = info.getUint32(16);
    final total = info.getUint32(20), offset = info.getUint32(24);
    final part = packet.length - 28;
    if (w == 0 ||
        h == 0 ||
        w > 16384 ||
        h > 16384 ||
        total == 0 ||
        total > maxPayload ||
        offset + part > total) {
      _fail('Invalid WebRTC frame dimensions');
      return;
    }
    if (_assemblyId != id || offset == 0) {
      if (offset != 0) return;
      _assemblyId = id;
      _assemblyWidth = w;
      _assemblyHeight = h;
      _assemblyLength = total;
      _assemblyOffset = 0;
      _assembly = Uint8List(total);
    }
    if (_assemblyLength != total ||
        _assemblyOffset != offset ||
        _assembly == null) {
      _assembly = null;
      _assemblyId = null;
      _assemblyOffset = 0;
      return;
    }
    _assembly!.setRange(offset, offset + part, packet, 28);
    _assemblyOffset += part;
    if (_assemblyOffset == total) {
      jpeg = _assembly;
      width = _assemblyWidth!;
      height = _assemblyHeight!;
      _frames++;
      status = 'Connected';
      _assembly = null;
      _assemblyId = null;
      _assemblyOffset = 0;
      final now = DateTime.now();
      final elapsed = now.difference(_fpsSince).inMilliseconds;
      if (elapsed >= 1000) {
        fps = (_frames * 1000 / elapsed).round();
        _frames = 0;
        _fpsSince = now;
      }
      if (!_disposed) notifyListeners();
    }
  }

  void _receive(List<int> chunk) {
    if (_socket == null) return;
    final data = Uint8List(_pending.length + chunk.length)
      ..setAll(0, _pending)
      ..setAll(_pending.length, chunk);
    var offset = 0;
    Uint8List? newest;
    var changed = false;
    while (data.length - offset >= 6) {
      if (data[offset] != 1) {
        _fail('Unsupported protocol version');
        return;
      }
      final header = ByteData.sublistView(data, offset, offset + 6);
      final type = data[offset + 1];
      final length = header.getUint32(2);
      if (length > maxPayload) {
        _fail('Frame too large');
        return;
      }
      if (data.length - offset < 6 + length) break;
      final start = offset + 6;
      final payload = Uint8List.sublistView(data, start, start + length);
      switch (type) {
        case authSuccess:
          connected = true;
          status = transport == TransportMode.webRtc
              ? 'Setting up WebRTC…'
              : 'Connected • waiting for Windows desktop';
          changed = true;
          if (transport == TransportMode.webRtc) unawaited(_startWebRtc());
          break;
        case authFailure:
          _fail('Incorrect password');
          return;
        case busy:
          _fail('Host is busy');
          return;
        case screenInfo:
          if (length != 8) {
            _fail('Invalid screen info');
            return;
          }
          final size = ByteData.sublistView(payload);
          width = size.getUint32(0);
          height = size.getUint32(4);
          changed = true;
          break;
        case frame:
          if (length < 20) {
            _fail('Invalid frame');
            return;
          }
          final info = ByteData.sublistView(payload);
          final w = info.getUint32(8), h = info.getUint32(12);
          final jpgLength = info.getUint32(16);
          if (w == 0 ||
              h == 0 ||
              w > 16384 ||
              h > 16384 ||
              jpgLength != length - 20) {
            _fail('Invalid frame dimensions');
            return;
          }
          width = w;
          height = h;
          newest = Uint8List.fromList(payload.sublist(20));
          _frames++;
          break;
        case snapshotListReply:
          if (length % 8 != 0) {
            _fail('Invalid screenshot list');
            return;
          }
          final ids = ByteData.sublistView(payload);
          snapshotIds = [
            for (var i = 0; i < length; i += 8) ids.getUint64(i),
          ];
          snapshotStatus = snapshotIds.isEmpty ? 'No screenshots yet' : null;
          changed = true;
          break;
        case snapshotFrame:
          if (length < 20) {
            snapshotStatus = 'Invalid screenshot';
          } else {
            final info = ByteData.sublistView(payload);
            final jpegLength = info.getUint32(16);
            if (jpegLength != length - 20) {
              snapshotStatus = 'Invalid screenshot';
            } else {
              snapshotJpeg = Uint8List.fromList(payload.sublist(20));
              snapshotStatus = null;
            }
          }
          changed = true;
          break;
        case snapshotError:
          snapshotStatus = 'Screenshot unavailable';
          changed = true;
          break;
        case webRtcAnswer:
          unawaited(_acceptWebRtcAnswer(payload));
          break;
        case ping:
          _send(pong, const []);
          break;
        case pong:
          break;
        case disconnectMessage:
          _fail('Host disconnected');
          return;
        default:
          _fail('Unknown message');
          return;
      }
      offset += 6 + length;
    }
    _pending = Uint8List.fromList(data.sublist(offset));
    if (newest != null) {
      jpeg = newest;
      status = 'Connected';
      changed = true;
    }
    final now = DateTime.now();
    final elapsed = now.difference(_fpsSince).inMilliseconds;
    if (elapsed >= 1000) {
      fps = (_frames * 1000 / elapsed).round();
      _frames = 0;
      _fpsSince = now;
      changed = true;
    }
    if (changed && !_disposed) notifyListeners();
  }

  void _fail(String message) {
    debugPrint('Viewer connection ended: $message');
    disconnect();
    status = message;
    if (!_disposed) notifyListeners();
  }

  void disconnect() {
    _webRtcRecoveryTimer?.cancel();
    _webRtcRecoveryTimer = null;
    _webRtcChannelOpen = false;
    final socket = _socket;
    if (socket != null) {
      _send(disconnectMessage, const []);
      socket.destroy();
    }
    _socket = null;
    final channel = _dataChannel;
    final peer = _peer;
    _dataChannel = null;
    _peer = null;
    unawaited(_closeWebRtc(channel, peer));
    _pending = Uint8List(0);
    jpeg = null;
    snapshotJpeg = null;
    snapshotIds = [];
    snapshotStatus = null;
    _assembly = null;
    _assemblyId = null;
    _assemblyOffset = 0;
    connected = false;
    fps = 0;
    if (!_disposed) notifyListeners();
  }

  Future<void> _closeWebRtc(
      RTCDataChannel? channel, RTCPeerConnection? peer) async {
    if (channel != null) {
      try {
        await channel.close();
      } catch (error) {
        debugPrint('Viewer data channel cleanup error: $error');
      }
    }
    if (peer != null) {
      try {
        await peer.close();
      } catch (error) {
        debugPrint('Viewer peer cleanup error: $error');
      }
    }
  }

  @override
  void dispose() {
    _disposed = true;
    disconnect();
    super.dispose();
  }
}
