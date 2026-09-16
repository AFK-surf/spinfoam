#include "spinfoam.h"

/* config: {url, keyword, deduplication_key}. web.read_chunk serves a stable snapshot:
 * input adds {offset, max_bytes}; result is {data, next_offset, done, snapshot}.
 * Later calls echo snapshot. data is UTF-8; matching is over its bytes. */
SF_MAIN int watch_page(void) {
    sf_handle config = sf_config();
    sf_handle keyword_value = sf_json_get(config, "keyword");
    char keyword[128], chunk[1024];
    unsigned short prefix[128];
    sf_i64 length = sf_json_read_string(keyword_value, keyword, sizeof(keyword));
    sf_drop(keyword_value);
    if (length <= 0 || length > 128) return 1;
    prefix[0] = 0;
    for (sf_i64 i = 1, j = 0; i < length; ++i) {
        while (j && keyword[i] != keyword[j]) j = prefix[j-1];
        if (keyword[i] == keyword[j]) ++j;
        prefix[i] = j;
    }
    for (;;) {
        sf_handle request = sf_config();
        sf_handle max_bytes = sf_json_number(sizeof(chunk));
        sf_json_set(request, "max_bytes", max_bytes); sf_drop(max_bytes);
        sf_i64 offset = 0, matched = 0;
        int found = 0;
        for (;;) {
            sf_handle number = sf_json_number(offset);
            sf_json_set(request, "offset", number); sf_drop(number);
            sf_handle reply = sf_host_call("web.read_chunk", request, 10000);
            if (reply < 0) {
                if (reply == SF_DENIED || reply == SF_INVALID) return 2;
                break; /* Retry a fresh snapshot after delay. */
            }
            sf_handle data = sf_json_get(reply, "data");
            sf_i64 bytes = sf_json_read_string(data, chunk, sizeof(chunk)); sf_drop(data);
            if (bytes < 0 || bytes > (sf_i64)sizeof(chunk)) { sf_drop(reply); break; }
            for (sf_i64 i = 0; i < bytes; ++i) {
                while (matched && chunk[i] != keyword[matched]) matched = prefix[matched-1];
                if (chunk[i] == keyword[matched]) ++matched;
                if (matched == length) { found = 1; break; }
            }
            if (found) { sf_drop(reply); break; }
            sf_handle done_value = sf_json_get(reply, "done");
            int done = sf_json_bool(done_value) == 1; sf_drop(done_value);
            sf_handle next_value = sf_json_get(reply, "next_offset");
            sf_i64 next = offset;
            int valid = sf_json_i64(next_value, &next) == 0; sf_drop(next_value);
            sf_handle snapshot = sf_json_get(reply, "snapshot");
            if (snapshot >= 0) { sf_json_set(request, "snapshot", snapshot); sf_drop(snapshot); }
            sf_drop(reply);
            if (done || !valid || next <= offset) break;
            offset = next;
        }
        sf_drop(request);
        if (found) {
            for (;;) {
                sf_handle ack = sf_host_call("agent.notify", config, 10000);
                if (ack >= 0) { sf_drop(ack); sf_drop(config); return 0; }
                if (ack == SF_DENIED || ack == SF_INVALID) return 3;
                sf_sleep_ms(5000);
            }
        }
        sf_sleep_ms(30000 + sf_now_mono_ms() % 1000);
    }
}
