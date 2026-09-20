# Remote Viewer MVP

View-only LAN desktop streaming. A Windows Service owns the TCP listener; an agent launched at interactive logon captures the primary display using DXGI Desktop Duplication, JPEG-encodes frames, and sends them to the service over loopback. The Flutter viewer runs on Linux and Android. There are no input-control protocol messages or remote-control handlers.

**LAN DEVELOPMENT VERSION. Authentication traffic is not encrypted. Do not expose port 5901 to the public internet.**

## Build

On a Windows 10/11 build machine, install the Rust toolchain with the MSVC target, Visual Studio C++ build tools, and NSIS. Run `powershell -ExecutionPolicy Bypass -File .\build-windows.ps1` from this repository. The output is `installer\RVHostSetup.exe`.

The checked-in source can also be cross-built on Linux with Rust's `x86_64-pc-windows-gnu` target, MinGW and NSIS: build `host` with `cargo build --release --target x86_64-pc-windows-gnu`, then run `makensis -DBUILDTARGET=x86_64-pc-windows-gnu installer/RVHost.nsi`.

On a Linux machine with Flutter and the Linux desktop development dependencies, run:

```sh
cd viewer
flutter pub get
flutter build linux
flutter build apk
```

The Linux executable and its shared libraries are under `viewer/build/linux/x64/release/bundle`. The Android APK is under `viewer/build/app/outputs/flutter-apk/app-release.apk`.
For this prototype, the Android release build uses Flutter's generated debug signing key.

The Android manifest in this repository grants Internet access for LAN TCP connections. The generated platform scaffolding supplies `MainActivity` and desktop launcher files.

## Install and use

Run the Windows installer as an administrator, enter a device name, port and password, then finish. The installer writes an Argon2id password hash in `password.hash` with service/admin access and a random local agent token in `config.json` with interactive-user read access, both under `%PROGRAMDATA%\RVHost`. It creates an automatic `RVHost` service, registers `RVCapture` under the machine-wide Run key, adds an inbound Windows Firewall rule for the configured TCP port, and starts both components. The rule applies to every Windows network profile so VM adapters work, while limiting remote addresses to the local subnet. `RVCapture` launches again at each interactive logon.

Open the viewer, enter the Windows host's LAN IPv4 address, configured port and password, then connect. One viewer can connect at a time. On Android, pinch and pan affect only the local image.

The installed service executable is `RVHost.exe`, and the interactive capture-agent executable is `RVCapture.exe`. If the viewer authenticates but remains at “waiting for Windows desktop,” confirm `RVCapture.exe` is running in the logged-in user's Task Manager session. Capture and IPC errors are written to `%LOCALAPPDATA%\RVHost\agent.log`. Service and viewer-session errors are written to `%PROGRAMDATA%\RVHost\host.log`.

## Protocol v1

Each message has a 6-byte header: version `u8` (1), type `u8`, and payload length `u32` big endian. Maximum payload is 16 MiB. Viewer types: `AUTH=1`, `AUTH_SUCCESS=2`, `AUTH_FAILURE=3`, `SCREEN_INFO=4`, `FRAME=5`, `PING=6`, `PONG=7`, `BUSY=8`, `DISCONNECT=9`. Authentication is a raw UTF-8 password payload. `SCREEN_INFO` contains width and height as big-endian `u32`. `FRAME` contains frame ID `u64`, width `u32`, height `u32`, JPEG length `u32`, then JPEG bytes. Agent IPC uses the same envelope on 127.0.0.1:45901, with a random token in its `AUTH` message.

The service stores a single latest frame; it replaces stale frames when capture outruns a viewer. No frame queue grows over time. A slow TCP write can still delay one frame for up to the three-second write timeout. The service tells the local capture agent to start only after a viewer authenticates and to stop after that viewer disconnects. While stopped, the agent releases DXGI resources and sleeps without polling. During an active session it drops byte-identical frames before color conversion and JPEG encoding.

## Current validation boundary

The Rust host cross-compiled for Windows, the NSIS installer compiled, the protocol unit test passed, and the Flutter analyzer, Linux build, and Android APK build passed. This Linux environment cannot run the Windows Service, DXGI capture, installer, or Android application. Before declaring the MVP complete, install on a Windows 10/11 host, reboot, confirm the service and agent start, and test connections from Linux and Android. Check password failure, second-viewer `BUSY`, display resolution change, logout and login, and viewer disconnect/reconnect.
