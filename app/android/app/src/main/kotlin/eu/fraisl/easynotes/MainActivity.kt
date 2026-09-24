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
    private var pendingText: Map<String, String?>? = null
    private var textSink: EventChannel.EventSink? = null

    override fun onCreate(savedInstanceState: android.os.Bundle?) {
        pendingLink = intent?.dataString
        // A relaunch after the process was killed replays the launching
        // intent; only a fresh launch is a new share.
        if (savedInstanceState == null) pendingText = sharedText(intent)
        super.onCreate(savedInstanceState)
    }

    /// The text and subject of an ACTION_SEND, or null for any other intent.
    private fun sharedText(intent: Intent?): Map<String, String?>? {
        if (intent?.action != Intent.ACTION_SEND) return null
        val text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()
        if (text.isNullOrBlank()) return null
        return mapOf(
            "text" to text,
            "subject" to intent.getStringExtra(Intent.EXTRA_SUBJECT),
        )
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
        MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            "eu.fraisl.easynotes/content_sharing",
        ).setMethodCallHandler { call, result ->
            if (call.method == "share") {
                val text = call.argument<String>("text")
                if (text == null) {
                    result.error("invalid_text", "Missing share text", null)
                } else {
                    val intent = Intent(Intent.ACTION_SEND).apply {
                        type = "text/plain"
                        putExtra(Intent.EXTRA_TEXT, text)
                    }
                    startActivity(Intent.createChooser(intent, null))
                    result.success(true)
                }
            } else {
                result.notImplemented()
            }
        }
        MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            "eu.fraisl.easynotes/shared_text",
        ).setMethodCallHandler { call, result ->
            if (call.method == "getInitialText") {
                result.success(pendingText)
                pendingText = null
            } else {
                result.notImplemented()
            }
        }
        EventChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            "eu.fraisl.easynotes/shared_text/events",
        ).setStreamHandler(object : EventChannel.StreamHandler {
            override fun onListen(arguments: Any?, events: EventChannel.EventSink?) {
                textSink = events
                pendingText?.let {
                    events?.success(it)
                    pendingText = null
                }
            }

            override fun onCancel(arguments: Any?) {
                textSink = null
            }
        })
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
        sharedText(intent)?.let { shared ->
            textSink?.success(shared) ?: run { pendingText = shared }
            return
        }
        val link = intent.dataString ?: return
        if (link.startsWith("easynotes://join#")) {
            linkSink?.success(link) ?: run { pendingLink = link }
        }
    }
}
