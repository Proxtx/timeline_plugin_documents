# Migration: MongoDB → new layout

## Old layout

This plugin never persisted events in MongoDB. Events were derived at
query time from filenames in each location's `diff_path` directory
(`<title>.diff.<unix_seconds>.pdf`). The on-disk three-tree layout
(`current_path`, `last_path`, `diff_path`) is what powered the diff
generation, and that layout is **unchanged**.

The RSA signing key used to sit at
`../plugins/timeline_plugin_documents/server/key` (PKCS#8 PEM). It now
auto-generates as a fresh PKCS#8 PEM at
`<data_dir>/plugins/timeline_plugin_documents/signing_key.pem` on first
run (overridable via `[config].signing_key_path` if you want to keep the
old key — see "Re-using the old key" below).

The server-side pdfjs static directory used to live at
`../plugins/timeline_plugin_documents/server/js/pdfjs/`. The client now
imports `pdfGen.js` directly from the wasm bundle, and pdfjs files are
served only when `[config].pdfjs_path` is set (point it at the same
directory you used before).

## Re-using the old signing key

```sh
cp /old/plugins/timeline_plugin_documents/server/key \
   <data_dir>/plugins/timeline_plugin_documents/signing_key.pem
```

…before launching the plugin for the first time. Otherwise a fresh key
is generated and any previously signed URLs cease to verify.

## Per-row conversion

There are no Mongo rows to convert for this plugin — everything is
derived from `diff_path/` filenames each time `/events` is queried.

## Notes

- `diff_path/` filenames must continue to follow the
  `<title>.diff.<unix_seconds>.pdf` convention — the plugin parses the
  trailing timestamp segment to place each diff onto the timeline.
- libpdfium.so still has to be locatable (set `[config].pdfium_path`
  or place `libpdfium.so` in CWD).
