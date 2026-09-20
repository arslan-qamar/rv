import 'dart:io';

import 'package:flutter/material.dart';

import 'protocol.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  PaintingBinding.instance.imageCache.maximumSize = 2;
  runApp(const RemoteViewerApp());
}

class RemoteViewerApp extends StatelessWidget {
  const RemoteViewerApp({super.key});
  @override
  Widget build(BuildContext context) => MaterialApp(
        title: 'Remote Viewer',
        theme: ThemeData(colorSchemeSeed: Colors.indigo, useMaterial3: true),
        home: const ViewerPage(),
      );
}

class ViewerPage extends StatefulWidget {
  const ViewerPage({super.key});
  @override
  State<ViewerPage> createState() => _ViewerPageState();
}

class _ViewerPageState extends State<ViewerPage> {
  final host = TextEditingController();
  final port = TextEditingController(text: '5901');
  final password = TextEditingController();
  final connection = RemoteConnection();
  final transform = TransformationController();

  @override
  void initState() {
    super.initState();
    connection.addListener(_refresh);
  }

  void _refresh() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    connection.removeListener(_refresh);
    connection.dispose();
    host.dispose();
    port.dispose();
    password.dispose();
    transform.dispose();
    super.dispose();
  }

  Future<void> _connect() async {
    final number = int.tryParse(port.text.trim());
    if (host.text.trim().isEmpty ||
        number == null ||
        number < 1 ||
        number > 65535) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('Enter a host and valid port')),
      );
      return;
    }
    transform.value = Matrix4.identity();
    await connection.connect(host.text.trim(), number, password.text);
    password.clear();
  }

  @override
  Widget build(BuildContext context) {
    final landscape =
        MediaQuery.orientationOf(context) == Orientation.landscape;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Remote Viewer'),
        actions: connection.connected
            ? [
                IconButton(
                  tooltip: 'Fit to screen',
                  onPressed: () => transform.value = Matrix4.identity(),
                  icon: const Icon(Icons.fit_screen),
                ),
                IconButton(
                  tooltip: 'Disconnect',
                  onPressed: () {
                    connection.disconnect();
                    transform.value = Matrix4.identity();
                  },
                  icon: const Icon(Icons.close),
                ),
              ]
            : null,
      ),
      body: connection.connected
          ? Column(
              children: [
                Expanded(
                  child: Container(
                    color: Colors.black,
                    child: Center(
                      child: connection.jpeg == null
                          ? Column(
                              mainAxisSize: MainAxisSize.min,
                              children: [
                                const CircularProgressIndicator(),
                                const SizedBox(height: 16),
                                Text(
                                  connection.status,
                                  style: const TextStyle(color: Colors.white),
                                ),
                              ],
                            )
                          : InteractiveViewer(
                              transformationController: transform,
                              minScale: 1,
                              maxScale: 6,
                              constrained: true,
                              child: SizedBox.expand(
                                child: Image.memory(
                                  connection.jpeg!,
                                  fit: BoxFit.contain,
                                  gaplessPlayback: true,
                                  filterQuality: FilterQuality.low,
                                ),
                              ),
                            ),
                    ),
                  ),
                ),
                if (!landscape)
                  Padding(
                    padding: const EdgeInsets.all(8),
                    child: Text(
                      'Connected • ${connection.fps} FPS • ${connection.width}×${connection.height}',
                    ),
                  ),
              ],
            )
          : Center(
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 420),
                child: Padding(
                  padding: const EdgeInsets.all(24),
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      TextField(
                        controller: host,
                        decoration: const InputDecoration(
                          labelText: 'Host',
                          hintText: '192.168.1.25',
                        ),
                      ),
                      TextField(
                        controller: port,
                        decoration: const InputDecoration(labelText: 'Port'),
                        keyboardType: TextInputType.number,
                      ),
                      TextField(
                        controller: password,
                        decoration: const InputDecoration(
                          labelText: 'Password',
                        ),
                        obscureText: true,
                        onSubmitted: (_) => _connect(),
                      ),
                      const SizedBox(height: 20),
                      FilledButton(
                        onPressed: _connect,
                        child: const Text('Connect'),
                      ),
                      const SizedBox(height: 12),
                      Text(connection.status),
                      if (Platform.isAndroid)
                        const Padding(
                          padding: EdgeInsets.only(top: 8),
                          child: Text(
                            'Pinch to zoom and drag to pan. Gestures stay on this device.',
                          ),
                        ),
                    ],
                  ),
                ),
              ),
            ),
    );
  }
}
