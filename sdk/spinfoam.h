#ifndef SPINFOAM_H
#define SPINFOAM_H
/* SDK ABI 1; BPF v3, little endian, 4096-byte stack frames. */
typedef unsigned long long sf_u64;
typedef long long sf_i64;
typedef sf_i64 sf_handle;
#define SF_MAIN __attribute__((section("spinfoam.main"), used))
#define SF_INLINE static __attribute__((always_inline)) inline
#define SF_INVALID (-1LL)
#define SF_LIMIT (-2LL)
#define SF_TIMEOUT (-3LL)
#define SF_DENIED (-4LL)
#define SF_HOST_ERROR (-5LL)
#define SF_CLOSED (-6LL)
#define SF_NULL 0
#define SF_BOOL 1
#define SF_NUMBER 2
#define SF_STRING 3
#define SF_ARRAY 4
#define SF_OBJECT 5
extern sf_i64 sf_sleep_ms(sf_u64 milliseconds);
extern sf_i64 sf_yield(void);
extern sf_u64 sf_now_mono_ms(void); /* milliseconds since this object's start */
extern sf_u64 sf_now_unix_ms(void);
extern sf_handle sf_config(void); /* new owned handle */
extern sf_i64 sf_drop(sf_handle handle);
extern sf_handle sf_event_next(sf_u64 timeout_ms); /* envelope {event_id,topic,payload} */
extern sf_handle sf_host_call_raw(const char *cap, sf_u64 cap_len, sf_handle params, sf_u64 timeout_ms);
extern sf_i64 sf_log_raw(const char *message, sf_u64 length);
extern sf_handle sf_json_parse(const char *json, sf_u64 length);
extern sf_handle sf_json_get_raw(sf_handle object, const char *key, sf_u64 length);
extern sf_handle sf_json_at(sf_handle array, sf_u64 index);
extern sf_i64 sf_json_kind(sf_handle value);
extern sf_i64 sf_json_i64(sf_handle value, sf_i64 *out);
extern sf_i64 sf_json_bool(sf_handle value);
/* Copies up to capacity bytes, no NUL. Returns full UTF-8 byte length. */
extern sf_i64 sf_json_read_string(sf_handle value, char *out, sf_u64 capacity);
extern sf_i64 sf_json_string_equals_raw(sf_handle object, const char *key, sf_u64 key_len, const char *value, sf_u64 value_len);
extern sf_handle sf_json_object(void);
extern sf_handle sf_json_array(void);
extern sf_handle sf_json_null(void);
extern sf_handle sf_json_string_raw(const char *value, sf_u64 length);
extern sf_handle sf_json_number(sf_i64 value);
extern sf_handle sf_json_boolean(sf_u64 value);
/* Set/push copy a value; caller retains ownership of both handles. */
extern sf_i64 sf_json_set_raw(sf_handle object, const char *key, sf_u64 length, sf_handle value);
extern sf_i64 sf_json_push(sf_handle array, sf_handle value);
extern sf_handle sf_json_dump(sf_handle value); /* owned UTF-8 bytes handle */
extern sf_handle sf_bytes(const void *bytes, sf_u64 length);
extern sf_i64 sf_bytes_len(sf_handle bytes); /* also string bytes / array or object count */
extern sf_i64 sf_bytes_read(sf_handle bytes, sf_u64 offset, void *out, sf_u64 capacity);
extern void *memcpy(void *dest, const void *source, unsigned long length);
extern void *memset(void *dest, int value, unsigned long length);
extern int memcmp(const void *a, const void *b, unsigned long length);
SF_INLINE sf_u64 sf_strlen(const char *s) { sf_u64 n=0; while(s[n]) ++n; return n; }
SF_INLINE sf_handle sf_host_call(const char *cap, sf_handle params, sf_u64 timeout_ms) { return sf_host_call_raw(cap,sf_strlen(cap),params,timeout_ms); }
SF_INLINE sf_i64 sf_log(const char *message) {return sf_log_raw(message,sf_strlen(message));}
SF_INLINE sf_handle sf_json_get(sf_handle h,const char *key) {return sf_json_get_raw(h,key,sf_strlen(key));}
SF_INLINE sf_handle sf_json_string(const char *value) {return sf_json_string_raw(value,sf_strlen(value));}
SF_INLINE sf_i64 sf_json_set(sf_handle h,const char *key,sf_handle value) {return sf_json_set_raw(h,key,sf_strlen(key),value);}
SF_INLINE sf_i64 sf_json_string_equals(sf_handle h,const char *key,const char *value) {return sf_json_string_equals_raw(h,key,sf_strlen(key),value,sf_strlen(value));}
#endif
