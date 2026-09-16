#include "spinfoam.h"

/* Accept arbitrary JSON envelopes. Notify for payload.kind == "deploy";
 * acknowledge all processed events using their stable event_id. */
SF_MAIN int handle_webhooks(void) {
    for (;;) {
        sf_handle event = sf_event_next(60000);
        if (event == SF_TIMEOUT) continue;
        if (event < 0) return 1;
        sf_handle payload = sf_json_get(event, "payload");
        int notify = sf_json_string_equals(payload, "kind", "deploy") == 1;
        sf_drop(payload);
        if (notify) {
            for (;;) {
                sf_handle ack = sf_host_call("agent.notify", event, 10000);
                if (ack >= 0) { sf_drop(ack); break; }
                if (ack == SF_DENIED || ack == SF_INVALID) return 2;
                sf_sleep_ms(5000);
            }
        }
        for (;;) {
            sf_handle ack = sf_host_call("webhook.ack", event, 10000);
            if (ack >= 0) { sf_drop(ack); break; }
            if (ack == SF_DENIED || ack == SF_INVALID) return 3;
            sf_sleep_ms(5000);
        }
        sf_drop(event);
    }
}
