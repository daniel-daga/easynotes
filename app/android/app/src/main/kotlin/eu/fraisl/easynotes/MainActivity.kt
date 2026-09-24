package eu.fraisl.easynotes

import android.content.Intent
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.EventChannel
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
    private val methodChannel = "eu.fraisl.easynotes/deep_links"
    private val eventChannel = "eu.fraisl.easynotes/deep_links/events"
    private var pendingLink: String? = null
    private var linkSink: EventChannel.EventSink? = null

    override fun onCreate(savedInstanceState: android.os.Bundle?) {
        pendingLink = intent?.dataString
        super.onCreate(savedInstanceState)
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, methodChannel)
            .setMethodCallHandler { call, result ->
                if (call.method == "getInitialLink") {
                    result.success(pendingLink)
                    pendingLink = null
                } else {
                    result.notImplemented()
                }
            }
        EventChannel(flutterEngine.dartExecutor.binaryMessenger, eventChannel)
            .setStreamHandler(object : EventChannel.StreamHandler {
                override fun onListen(arguments: Any?, events: EventChannel.EventSink?) {
                    linkSink = events
                    pendingLink?.let {
                        events?.success(it)
                        pendingLink = null
                    }
                }

                override fun onCancel(arguments: Any?) {
                    linkSink = null
                }
            })
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        val link = intent.dataString ?: return
        if (link.startsWith("easynotes://join#")) {
            linkSink?.success(link) ?: run { pendingLink = link }
        }
    }
}
