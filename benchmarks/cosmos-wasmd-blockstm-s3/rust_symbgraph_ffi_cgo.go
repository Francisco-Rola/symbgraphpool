//go:build acg_rust

package main

/*
#cgo CFLAGS: -I${SRCDIR}/../../runtime/crates/acg-wasmd-scheduler-ffi/include
#cgo LDFLAGS: -L${SRCDIR}/../../runtime/target/release -lacg_wasmd_scheduler_ffi -ldl -lm -lpthread
#include "acg_wasmd_scheduler_ffi.h"
*/
import "C"

import (
	"encoding/json"
	"fmt"
	"runtime"
	"time"
	"unsafe"
)

type cgoRustScheduler struct {
	ptr *C.AcgWasmdScheduler
}

func cBufferBytes(buffer C.AcgByteBuffer) []byte {
	if buffer.ptr == nil || buffer.len == 0 {
		return nil
	}
	return C.GoBytes(unsafe.Pointer(buffer.ptr), C.int(buffer.len))
}

func cBufferError(buffer C.AcgByteBuffer) error {
	defer C.acg_wasmd_scheduler_buffer_free(buffer)
	if buffer.ptr == nil {
		return fmt.Errorf("Rust ACG bridge failed without an error payload")
	}
	return fmt.Errorf("Rust ACG bridge: %s", string(cBufferBytes(buffer)))
}

func jsonPointer(bz []byte) (*C.uint8_t, C.size_t) {
	if len(bz) == 0 {
		return nil, 0
	}
	return (*C.uint8_t)(unsafe.Pointer(&bz[0])), C.size_t(len(bz))
}

func newRustSchedulerFFI(config rustBridgeConfig) (rustSchedulerFFI, error) {
	bz, err := json.Marshal(config)
	if err != nil {
		return nil, err
	}
	ptr, length := jsonPointer(bz)
	var cErr C.AcgByteBuffer
	scheduler := C.acg_wasmd_scheduler_new(ptr, length, &cErr)
	runtime.KeepAlive(bz)
	if scheduler == nil {
		return nil, cBufferError(cErr)
	}
	return &cgoRustScheduler{ptr: scheduler}, nil
}

func (s *cgoRustScheduler) callPlan(request rustPlanRequest) (C.AcgByteBuffer, rustBridgePlanTimings, error) {
	var timings rustBridgePlanTimings
	marshalStarted := time.Now()
	bz, err := json.Marshal(request)
	if err != nil {
		return C.AcgByteBuffer{}, timings, err
	}
	timings.RequestMarshalNanos = uint64(time.Since(marshalStarted).Nanoseconds())
	ptr, length := jsonPointer(bz)
	var out, cErr C.AcgByteBuffer
	callStarted := time.Now()
	status := C.acg_wasmd_scheduler_plan(s.ptr, ptr, length, &out, &cErr)
	timings.CGORoundTripNanos = uint64(time.Since(callStarted).Nanoseconds())
	runtime.KeepAlive(bz)
	if status != 0 {
		return C.AcgByteBuffer{}, timings, cBufferError(cErr)
	}
	return out, timings, nil
}

func (s *cgoRustScheduler) plan(request rustPlanRequest) (rustPlanResponse, error) {
	out, timings, err := s.callPlan(request)
	if err != nil {
		return rustPlanResponse{}, err
	}
	defer C.acg_wasmd_scheduler_buffer_free(out)
	var response rustPlanResponse
	unmarshalStarted := time.Now()
	if err := json.Unmarshal(cBufferBytes(out), &response); err != nil {
		return rustPlanResponse{}, fmt.Errorf("decode Rust ACG plan: %w", err)
	}
	timings.ResponseUnmarshalNanos = uint64(time.Since(unmarshalStarted).Nanoseconds())
	response.BridgeTimings = timings
	return response, nil
}

func (s *cgoRustScheduler) feedback(request rustFeedbackRequest) (rustFeedbackResponse, error) {
	bz, err := json.Marshal(request)
	if err != nil {
		return rustFeedbackResponse{}, err
	}
	ptr, length := jsonPointer(bz)
	var out, cErr C.AcgByteBuffer
	status := C.acg_wasmd_scheduler_feedback(s.ptr, ptr, length, &out, &cErr)
	runtime.KeepAlive(bz)
	if status != 0 {
		return rustFeedbackResponse{}, cBufferError(cErr)
	}
	defer C.acg_wasmd_scheduler_buffer_free(out)
	var response rustFeedbackResponse
	if err := json.Unmarshal(cBufferBytes(out), &response); err != nil {
		return rustFeedbackResponse{}, fmt.Errorf("decode Rust ACG feedback: %w", err)
	}
	return response, nil
}

func (s *cgoRustScheduler) close() {
	if s.ptr != nil {
		C.acg_wasmd_scheduler_free(s.ptr)
		s.ptr = nil
	}
}
