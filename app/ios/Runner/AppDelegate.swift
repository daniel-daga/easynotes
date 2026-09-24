import Flutter
import UIKit

@main
@objc class AppDelegate: FlutterAppDelegate {
  private var pendingLink: String?
  private var linkSink: FlutterEventSink?

  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    pendingLink = (launchOptions?[.url] as? URL)?.absoluteString
    GeneratedPluginRegistrant.register(with: self)
    let launched = super.application(application, didFinishLaunchingWithOptions: launchOptions)
    if let controller = window?.rootViewController as? FlutterViewController {
      let methods = FlutterMethodChannel(
        name: "eu.fraisl.easynotes/deep_links",
        binaryMessenger: controller.binaryMessenger
      )
      methods.setMethodCallHandler { [weak self] call, result in
        if call.method == "getInitialLink" {
          result(self?.pendingLink)
          self?.pendingLink = nil
        } else {
          result(FlutterMethodNotImplemented)
        }
      }
      let sharing = FlutterMethodChannel(
        name: "eu.fraisl.easynotes/content_sharing",
        binaryMessenger: controller.binaryMessenger
      )
      sharing.setMethodCallHandler { [weak controller] call, result in
        guard call.method == "share" else {
          result(FlutterMethodNotImplemented)
          return
        }
        guard let arguments = call.arguments as? [String: Any],
              let text = arguments["text"] as? String else {
          result(FlutterError(code: "invalid_text", message: "Missing share text", details: nil))
          return
        }
        let sheet = UIActivityViewController(activityItems: [text], applicationActivities: nil)
        if let popover = sheet.popoverPresentationController, let view = controller?.view {
          popover.sourceView = view
          popover.sourceRect = CGRect(x: view.bounds.midX, y: view.bounds.midY, width: 0, height: 0)
        }
        controller?.present(sheet, animated: true)
        result(true)
      }
      let events = FlutterEventChannel(
        name: "eu.fraisl.easynotes/deep_links/events",
        binaryMessenger: controller.binaryMessenger
      )
      events.setStreamHandler(DeepLinkStreamHandler(
        onListen: { [weak self] sink in
          self?.linkSink = sink
          if let link = self?.pendingLink {
            sink?(link)
            self?.pendingLink = nil
          }
        },
        onCancel: { [weak self] in self?.linkSink = nil }
      ))
    }
    return launched
  }

  override func application(
    _ app: UIApplication,
    open url: URL,
    options: [UIApplication.OpenURLOptionsKey: Any] = [:]
  ) -> Bool {
    let link = url.absoluteString
    if link.hasPrefix("easynotes://join#") {
      if let sink = linkSink {
        sink(link)
      } else {
        pendingLink = link
      }
    }
    return super.application(app, open: url, options: options)
  }
}

private final class DeepLinkStreamHandler: NSObject, FlutterStreamHandler {
  private let onListen: (FlutterEventSink?) -> Void
  private let onCancel: () -> Void

  init(onListen: @escaping (FlutterEventSink?) -> Void, onCancel: @escaping () -> Void) {
    self.onListen = onListen
    self.onCancel = onCancel
  }

  func onListen(withArguments arguments: Any?, eventSink events: @escaping FlutterEventSink) -> FlutterError? {
    onListen(events)
    return nil
  }

  func onCancel(withArguments arguments: Any?) -> FlutterError? {
    onCancel()
    return nil
  }
}
