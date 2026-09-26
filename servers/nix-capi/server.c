// nix-pbt evaluation server for CppNix, using the Nix C API.
//
// Speaks the nix-pbt server protocol on stdin/stdout (see src/eval.rs):
//
//   request:  <byte length>\n<expression>
//   response: ok <byte length>\n<JSON>          (the value, as builtins.toJSON prints it)
//          |  error <byte length>\n<message>
//
// Usage: nix-pbt-server-capi [--store URI] [--option NAME VALUE]...

#include <nix_api_expr.h>
#include <nix_api_flake.h>
#include <nix_api_store.h>
#include <nix_api_util.h>
#include <nix_api_value.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct buf {
    char * data;
    size_t len;
};

static void append_cb(const char * start, unsigned int n, void * user_data)
{
    struct buf * b = user_data;
    b->data = realloc(b->data, b->len + n + 1);
    memcpy(b->data + b->len, start, n);
    b->len += n;
    b->data[b->len] = 0;
}

static void respond(const char * status, const char * data, size_t len)
{
    printf("%s %zu\n", status, len);
    fwrite(data, 1, len, stdout);
    fflush(stdout);
}

static void respond_error(nix_c_context * ctx)
{
    unsigned int n = 0;
    const char * msg = nix_err_msg(NULL, ctx, &n);
    if (!msg)
        msg = "unknown error", n = strlen(msg);
    respond("error", msg, n);
}

static void die(nix_c_context * ctx, const char * what)
{
    fprintf(stderr, "nix-pbt-server-capi: %s: %s\n", what, nix_err_msg(NULL, ctx, NULL));
    exit(2);
}

int main(int argc, char ** argv)
{
    nix_c_context * ctx = nix_c_context_create();
    const char * store_uri = NULL;

    if (nix_libutil_init(ctx) != NIX_OK)
        die(ctx, "libutil init");
    if (nix_libstore_init(ctx) != NIX_OK)
        die(ctx, "libstore init");
    if (nix_libexpr_init(ctx) != NIX_OK)
        die(ctx, "libexpr init");

    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--store") && i + 1 < argc) {
            store_uri = argv[++i];
        } else if (!strcmp(argv[i], "--option") && i + 2 < argc) {
            if (nix_setting_set(ctx, argv[i + 1], argv[i + 2]) != NIX_OK)
                die(ctx, argv[i + 1]);
            i += 2;
        } else {
            fprintf(stderr, "usage: %s [--store URI] [--option NAME VALUE]...\n", argv[0]);
            return 2;
        }
    }

    Store * store = nix_store_open(ctx, store_uri, NULL);
    if (!store)
        die(ctx, "opening store");
    nix_eval_state_builder * builder = nix_eval_state_builder_new(ctx, store);
    if (!builder || nix_eval_state_builder_load(ctx, builder) != NIX_OK)
        die(ctx, "eval state builder");
    // Registers builtins.getFlake, parseFlakeRef, flakeRefToString (when the
    // flakes feature is enabled).
    nix_flake_settings * flake_settings = nix_flake_settings_new(ctx);
    if (!flake_settings || nix_flake_settings_add_to_eval_state_builder(ctx, flake_settings, builder) != NIX_OK)
        die(ctx, "flake settings");
    EvalState * state = nix_eval_state_build(ctx, builder);
    if (!state)
        die(ctx, "eval state");
    nix_eval_state_builder_free(builder);

    nix_value * to_json = nix_alloc_value(ctx, state);
    if (nix_expr_eval_from_string(ctx, state, "builtins.toJSON", ".", to_json) != NIX_OK)
        die(ctx, "builtins.toJSON");

    char * cwd = getenv("PWD");
    size_t len;
    while (scanf("%zu", &len) == 1) {
        if (getchar() != '\n')
            break;
        char * expr = malloc(len + 1);
        if (fread(expr, 1, len, stdin) != len)
            break;
        expr[len] = 0;

        nix_value * v = nix_alloc_value(ctx, state);
        nix_value * json = nix_alloc_value(ctx, state);
        struct buf out = {0};
        if (nix_expr_eval_from_string(ctx, state, expr, cwd ? cwd : ".", v) != NIX_OK
            || nix_value_call(ctx, state, to_json, v, json) != NIX_OK
            || nix_value_force(ctx, state, json) != NIX_OK
            || nix_get_string(ctx, json, append_cb, &out) != NIX_OK) {
            respond_error(ctx);
        } else {
            respond("ok", out.data ? out.data : "", out.len);
        }
        free(out.data);
        free(expr);
        nix_value_decref(ctx, v);
        nix_value_decref(ctx, json);
        nix_clear_err(ctx);
    }
    return 0;
}
