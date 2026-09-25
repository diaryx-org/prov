// The C side of `Ws.loads`. The adapter is `ws_dirs.c`'s; the compiler
// splices both into one program, so the declarations are enough.
#include <stdint.h>

typedef int32_t (*PbCall)(const char* in, size_t in_len, char** out, size_t* out_len);
static Term pb_run(Env e, Term arg, IoWork* w, PbCall call);

extern int32_t pb_loads(const char* in, size_t in_len, char** out, size_t* out_len);

Term ws_loads_run(Env e, Term* f, IoWork* w) {
  return pb_run(e, f[0], w, pb_loads);
}

static void __attribute__((constructor)) ws_loads_use(void) {
  io_eff(CID_WS_LOADS, ws_loads_run, 0);
}
