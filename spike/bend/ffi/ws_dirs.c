// The C side of `Ws.dirs` and `Ws.loads`: copy the argument out of Bend's
// heap, hand it to Rust on a helper thread, and rebuild the answer on the
// event loop. `../RUNTIME.md` is the shape; `src/lib.rs` the ABI. (historica's
// spike wrote this adapter first; it is the same three functions.)
//
// Both effects are one function apart, so the adapter is written once, here,
// and `ws_loads.c` — spliced into the same C file — declares it.

#include <stdint.h>

typedef int32_t (*PbCall)(const char* in, size_t in_len, char** out, size_t* out_len);

extern void pb_free(char* p, size_t len);

// `w->data` holds the argument going in and the answer coming out; `w->hand`
// carries the Rust function to the helper thread, which has no other scratch.
static void pb_call(IoWork* w) {
  char* out = NULL; size_t n = 0;
  w->code = (u32)((PbCall)w->hand)((const char*)w->data, w->size, &out, &n);
  free(w->data);
  w->data = out; w->size = n;
}

// The answer is pieces separated by 0x1e (`RS` in `src/lib.rs`); Bend gets
// them as a list of strings, so each piece can be read on a lane of its own
// rather than all of them walked on the event loop's.
static Term pb_pack(Env e, IoWork* w) {
  Term r;
  if (w->code) {
    r = io_fail(e, w->code, w->data);
  } else {
    Term xs = term_pak(CID_NIL, 0);
    u64 end = w->size;
    for (u64 i = w->size; ; i -= 1) {
      if (i == 0 || w->data[i - 1] == 0x1e) {
        xs = io_node(e, CID_CON, io_str(e, w->data + i, end - i), xs);
        end = i - 1;
        if (i == 0) break;
      }
    }
    r = io_done(e, xs);
  }
  pb_free(w->data, w->size);
  return r;
}

static Term pb_run(Env e, Term arg, IoWork* w, PbCall call) {
  u64 n; w->data = io_cstr(e, arg, &n); w->size = n;
  w->hand = (intptr_t)call;
  return io_work(w, pb_call, pb_pack);
}

extern int32_t pb_dirs(const char* in, size_t in_len, char** out, size_t* out_len);

Term ws_dirs_run(Env e, Term* f, IoWork* w) {
  return pb_run(e, f[0], w, pb_dirs);
}

static void __attribute__((constructor)) ws_dirs_use(void) {
  io_eff(CID_WS_DIRS, ws_dirs_run, 0);
}
