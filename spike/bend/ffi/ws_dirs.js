// The JS twin of `ws_dirs.c`. The document parse is fig and twig, native
// libraries a JS build cannot reach, so this build refuses in words rather
// than answering differently from the native one.
function ws_dirs(query) {
  return { $: "Fail", error: io_tup(95, "prov-bend: the JavaScript build has no host services; build it natively (python3 check.py)") };
}
