#include "spinfoam.h"

/* config: {repository, run_id, deduplication_key}. The embedder owns credentials. */
SF_MAIN int monitor_run(void) {
    sf_handle config = sf_config();
    if (config < 0) return 1;
    sf_u64 delay = 15000;
    for (;;) {
        sf_handle run = sf_host_call("github.run.read", config, 10000);
        if (run >= 0) {
            if (sf_json_string_equals(run, "status", "completed") == 1) {
                /* Retain the same payload/key across ambiguous notification timeouts. */
                if (sf_json_set(config, "run", run) < 0) { sf_drop(run); return 2; }
                sf_drop(run);
                for (;;) {
                    sf_handle ack = sf_host_call("agent.notify", config, 10000);
                    if (ack >= 0) { sf_drop(ack); sf_drop(config); return 0; }
                    if (ack == SF_DENIED || ack == SF_INVALID) { sf_drop(config); return 3; }
                    sf_sleep_ms(5000);
                }
            }
            sf_drop(run);
            delay = 15000;
        } else {
            if (run == SF_DENIED || run == SF_INVALID) { sf_drop(config); return 4; }
            if (delay < 60000) delay *= 2;
            if (delay > 60000) delay = 60000;
        }
        sf_sleep_ms(delay + sf_now_mono_ms() % 1000);
    }
}
