//go:build !acg_rust

package main

import "fmt"

type unavailableRustScheduler struct{}

func newRustSchedulerFFI(rustBridgeConfig) (rustSchedulerFFI, error) {
	return nil, fmt.Errorf("Rust SymbGraph bridge is not linked; build runtime/crates/acg-wasmd-scheduler-ffi --release and run Go with -tags acg_rust")
}

func (unavailableRustScheduler) plan(rustPlanRequest) (rustPlanResponse, error) {
	return rustPlanResponse{}, fmt.Errorf("Rust SymbGraph bridge unavailable")
}
func (unavailableRustScheduler) feedback(rustFeedbackRequest) (rustFeedbackResponse, error) {
	return rustFeedbackResponse{}, fmt.Errorf("Rust SymbGraph bridge unavailable")
}
func (unavailableRustScheduler) close() {}
