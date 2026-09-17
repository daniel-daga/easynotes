// Custom bootstrap, so the app loads its renderer from its own origin.
//
// Flutter's default fetches CanvasKit from gstatic.com on every page load. For a
// product whose whole claim is that the server learns nothing, announcing each
// launch to a third-party CDN is not acceptable — and it means the app will not
// load at all behind a network that blocks it. Flutter already copies the files
// into `canvaskit/` next to the build; this points at those.
{{flutter_js}}
{{flutter_build_config}}

_flutter.loader.load({
  config: {
    canvasKitBaseUrl: "canvaskit/",
  },
  serviceWorkerSettings: {
    serviceWorkerVersion: {{flutter_service_worker_version}},
  },
});
