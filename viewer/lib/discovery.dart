import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:multicast_dns/multicast_dns.dart';

const _serviceType = '_rvhost._tcp.local';
const _androidChannel = MethodChannel('com.example.remote_viewer/discovery');

class DiscoveredHost {
  const DiscoveredHost({
    required this.id,
    required this.name,
    required this.address,
    required this.port,
  });

  final String id;
  final String name;
  final String address;
  final int port;
}

class HostDiscovery extends ChangeNotifier {
  List<DiscoveredHost> hosts = const [];
  bool scanning = false;
  String? error;
  bool _disposed = false;

  Future<void> scan() async {
    if (scanning) return;
    scanning = true;
    hosts = const [];
    error = null;
    _notify();

    final client = MDnsClient();
    var multicastLockHeld = false;
    try {
      if (Platform.isAndroid) {
        await _androidChannel.invokeMethod<void>('acquireMulticastLock');
        multicastLockHeld = true;
      }
      await client.start();
      final pointers = await client
          .lookup<PtrResourceRecord>(
            ResourceRecordQuery.serverPointer(_serviceType),
            timeout: const Duration(seconds: 3),
          )
          .toList();
      final uniquePointers = <String, PtrResourceRecord>{
        for (final pointer in pointers) pointer.domainName: pointer,
      };
      final resolved = await Future.wait(
        uniquePointers.values.map((pointer) => _resolve(client, pointer)),
      );
      hosts = resolved.whereType<DiscoveredHost>().toList()
        ..sort((a, b) => a.name.toLowerCase().compareTo(b.name.toLowerCase()));
    } catch (exception) {
      error = 'Network discovery unavailable';
      debugPrint('mDNS discovery failed: $exception');
    } finally {
      client.stop();
      if (multicastLockHeld) {
        try {
          await _androidChannel.invokeMethod<void>('releaseMulticastLock');
        } catch (_) {
          // The activity may have been destroyed while the query was running.
        }
      }
      scanning = false;
      _notify();
    }
  }

  Future<DiscoveredHost?> _resolve(
    MDnsClient client,
    PtrResourceRecord pointer,
  ) async {
    final services = await client
        .lookup<SrvResourceRecord>(
          ResourceRecordQuery.service(pointer.domainName),
          timeout: const Duration(seconds: 2),
        )
        .toList();
    if (services.isEmpty) return null;
    final service = services.first;
    final addressResults = await Future.wait([
      client
          .lookup<IPAddressResourceRecord>(
            ResourceRecordQuery.addressIPv4(service.target),
            timeout: const Duration(seconds: 2),
          )
          .toList(),
      client
          .lookup<IPAddressResourceRecord>(
            ResourceRecordQuery.addressIPv6(service.target),
            timeout: const Duration(seconds: 2),
          )
          .toList(),
    ]);
    final records = [...addressResults[0], ...addressResults[1]];
    if (records.isEmpty) return null;

    var name = pointer.domainName;
    final suffix = '.$_serviceType';
    if (name.toLowerCase().endsWith(suffix)) {
      name = name.substring(0, name.length - suffix.length);
    }
    return DiscoveredHost(
      id: pointer.domainName,
      name: name,
      address: records.first.address.address,
      port: service.port,
    );
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}
