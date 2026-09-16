#include "spinfoam.h"

/* config: {entity_id, desired_state}. The embedder forwards authenticated device events. */
SF_MAIN int monitor_device(void) {
    sf_handle config = sf_config();
    sf_handle desired = sf_json_get(config, "desired_state");
    char state[256];
    sf_i64 length = sf_json_read_string(desired, state, sizeof(state));
    sf_drop(desired);
    sf_drop(config);
    if (length < 0 || length > (sf_i64)sizeof(state)) return 1;
    int previously_matched = 0;
    for (;;) {
        sf_handle event = sf_event_next(60000);
        if (event == SF_TIMEOUT) continue;
        if (event < 0) return 2;
        sf_handle payload = sf_json_get(event, "payload");
        int matched = sf_json_string_equals_raw(payload, "state", 5, state, length) == 1;
        sf_drop(payload);
        if (matched && !previously_matched) {
            for (;;) {
                sf_handle ack = sf_host_call("agent.notify", event, 10000);
                if (ack >= 0) { sf_drop(ack); break; }
                if (ack == SF_DENIED || ack == SF_INVALID) { sf_drop(event); return 3; }
                sf_sleep_ms(5000);
            }
        }
        previously_matched = matched;
        sf_drop(event);
    }
}
