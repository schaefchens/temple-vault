// What temple_ffi.framework exports for TempleReader.swift (bindings/ffi/src/native.rs). Swift
// sees these as __temple_reader_…: TempleReader is the API.

#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// 0, or the error's kind – 1 Locked, 2 NoSuchObject, 3 Damaged, 4 Database, 5 Io – with its
// message in `message`, at most `capacity` bytes with the closing NUL.
int32_t temple_reader_object_length(const char *handle, int64_t id, uint64_t *length, char *message,
                                    size_t capacity) __attribute__((swift_private));

// `*count` bytes at `*bytes`, which temple_reader_free wipes and frees.
int32_t temple_reader_read_object(const char *handle, int64_t id, uint64_t start, uint64_t end, uint8_t **bytes,
                                  size_t *count, char *message, size_t capacity) __attribute__((swift_private));

void temple_reader_free(uint8_t *bytes, size_t count) __attribute__((swift_private));

#ifdef __cplusplus
}
#endif
