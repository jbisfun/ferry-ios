// C ABI of the Ferry core (core/src/lib.rs). Requests, results and events are
// JSON documents described in core/src/protocol.rs.
#pragma once
#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/// Called for every event (result, error, progress, prompt, log), on a core thread.
typedef void (*ferry_core_callback)(const char *json, void *ctx);

/// Start the core with a protocol::Config JSON. Returns false on bad config or if already started.
bool ferry_core_start(const char *config_json, ferry_core_callback callback, void *ctx);

/// Queue a request `{"method": "...", "params": {...}}`; returns its id (0 = not started).
uint64_t ferry_core_request(const char *request_json);

/// Answer a prompt event.
bool ferry_core_respond(uint64_t prompt, const char *answer_json);

/// Cancel a running request; it ends with an error event.
void ferry_core_cancel(uint64_t id);

/// JSON array of method names. Free with ferry_core_free.
char *ferry_core_methods(void);

void ferry_core_free(char *p);

#ifdef __cplusplus
}
#endif
