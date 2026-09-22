import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';

const int auth = 1, authSuccess = 2, authFailure = 3, screenInfo = 4;
const int frame = 5, ping = 6, pong = 7, busy = 8, disconnectMessage = 9;
const int snapshotList = 10, snapshotListReply = 11, snapshotGet = 12;
const int snapshotFrame = 13, snapshotError = 14;
const int maxPayload = 16 * 1024 * 1024;

class RemoteConnection extends ChangeNotifier {
  Socket? _socket;
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

  Future<void> connect(String host, int port, String password) async {
    disconnect();
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
          _fail('Connection error: $error');
        },
        onDone: () {
          if (_socket == socket) _fail('Disconnected');
        },
        cancelOnError: true,
      );
      _send(auth, utf8.encode(password));
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
          status = 'Connected • waiting for Windows desktop';
          changed = true;
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
    disconnect();
    status = message;
    if (!_disposed) notifyListeners();
  }

  void disconnect() {
    final socket = _socket;
    if (socket != null) {
      _send(disconnectMessage, const []);
      socket.destroy();
    }
    _socket = null;
    _pending = Uint8List(0);
    jpeg = null;
    snapshotJpeg = null;
    snapshotIds = [];
    snapshotStatus = null;
    connected = false;
    fps = 0;
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    disconnect();
    super.dispose();
  }
}
