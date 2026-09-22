package com.example.remote_viewer

import android.content.Context
import android.net.wifi.WifiManager
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
    private var multicastLock: WifiManager.MulticastLock? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            "com.example.remote_viewer/discovery",
        ).setMethodCallHandler { call, result ->
            when (call.method) {
                "acquireMulticastLock" -> {
                    val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
                    if (multicastLock == null) {
                        multicastLock = wifi.createMulticastLock("RVHost discovery").apply {
                            setReferenceCounted(false)
                        }
                    }
                    if (multicastLock?.isHeld == false) multicastLock?.acquire()
                    result.success(null)
                }
                "releaseMulticastLock" -> {
                    if (multicastLock?.isHeld == true) multicastLock?.release()
                    result.success(null)
                }
                else -> result.notImplemented()
            }
        }
    }

    override fun onDestroy() {
        if (multicastLock?.isHeld == true) multicastLock?.release()
        super.onDestroy()
    }
}
