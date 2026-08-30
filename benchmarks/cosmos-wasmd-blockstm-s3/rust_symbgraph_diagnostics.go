package main

import (
	"fmt"
	"os"
	"strconv"
	"strings"
)

func rustEnvOr(name, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(name)); value != "" {
		return value
	}
	return fallback
}

func rustOptionalFloat(name, raw string) (*float64, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return nil, nil
	}
	value, err := strconv.ParseFloat(raw, 64)
	if err != nil {
		return nil, fmt.Errorf("invalid %s=%q: %w", name, raw, err)
	}
	return &value, nil
}

func rustOptionalUint32(name, raw string) (*uint32, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return nil, nil
	}
	value, err := strconv.ParseUint(raw, 10, 32)
	if err != nil {
		return nil, fmt.Errorf("invalid %s=%q: %w", name, raw, err)
	}
	out := uint32(value)
	return &out, nil
}

func rustOptionalInt(name, raw string) (*int, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return nil, nil
	}
	value, err := strconv.Atoi(raw)
	if err != nil || value < 0 {
		if err == nil {
			err = fmt.Errorf("must be non-negative")
		}
		return nil, fmt.Errorf("invalid %s=%q: %w", name, raw, err)
	}
	return &value, nil
}

func rustPlanningOverridesFromStrings(edgeMaterialization, softThreshold, hardThreshold, riskBudget, explorationRate, explorationRiskBudget, explorationMinUncertainty, explorationMaxTransactions, independenceBeforeSoftening, softeningMinConfidence string) (*rustPlanningOverrides, error) {
	var out rustPlanningOverrides
	var err error
	if out.EdgeMaterializationThreshold, err = rustOptionalFloat("edge-materialization-threshold", edgeMaterialization); err != nil {
		return nil, err
	}
	if out.SoftThreshold, err = rustOptionalFloat("soft-threshold", softThreshold); err != nil {
		return nil, err
	}
	if out.HardThreshold, err = rustOptionalFloat("hard-threshold", hardThreshold); err != nil {
		return nil, err
	}
	if out.RiskBudget, err = rustOptionalFloat("risk-budget", riskBudget); err != nil {
		return nil, err
	}
	if out.ExplorationRate, err = rustOptionalFloat("exploration-rate", explorationRate); err != nil {
		return nil, err
	}
	if out.ExplorationRiskBudget, err = rustOptionalFloat("exploration-risk-budget", explorationRiskBudget); err != nil {
		return nil, err
	}
	if out.ExplorationMinUncertainty, err = rustOptionalFloat("exploration-min-uncertainty", explorationMinUncertainty); err != nil {
		return nil, err
	}
	if out.ExplorationMaxTransactionsPerBlock, err = rustOptionalInt("exploration-max-transactions", explorationMaxTransactions); err != nil {
		return nil, err
	}
	if out.IndependentObservationsBeforeSoftening, err = rustOptionalUint32("independence-before-softening", independenceBeforeSoftening); err != nil {
		return nil, err
	}
	if out.SofteningMinConfidence, err = rustOptionalFloat("softening-min-confidence", softeningMinConfidence); err != nil {
		return nil, err
	}
	if out.EdgeMaterializationThreshold == nil && out.SoftThreshold == nil && out.HardThreshold == nil && out.RiskBudget == nil && out.ExplorationRate == nil && out.ExplorationRiskBudget == nil && out.ExplorationMinUncertainty == nil && out.ExplorationMaxTransactionsPerBlock == nil && out.IndependentObservationsBeforeSoftening == nil && out.SofteningMinConfidence == nil {
		return nil, nil
	}
	return &out, nil
}

const (
	rustVisibilityMVCC         = "mvcc"
	rustVisibilityMaterialized = "materialized"
	rustValidationIndexed      = "indexed"
	rustValidationScan         = "scan"
	rustFeedbackProfile        = "profile"
	rustFeedbackAllPairs       = "all-pairs"
)

type RustSymbGraphRunnerOptions struct {
	Visibility string
	Validation string
	Feedback   string
}

func DefaultRustSymbGraphRunnerOptions() RustSymbGraphRunnerOptions {
	return RustSymbGraphRunnerOptions{
		Visibility: rustVisibilityMVCC,
		Validation: rustValidationIndexed,
		Feedback:   rustFeedbackProfile,
	}
}

func LegacyRustSymbGraphRunnerOptions() RustSymbGraphRunnerOptions {
	return RustSymbGraphRunnerOptions{
		Visibility: rustVisibilityMaterialized,
		Validation: rustValidationScan,
		Feedback:   rustFeedbackAllPairs,
	}
}

func (o RustSymbGraphRunnerOptions) Normalize() (RustSymbGraphRunnerOptions, error) {
	if strings.TrimSpace(o.Visibility) == "" {
		o.Visibility = rustVisibilityMVCC
	}
	if strings.TrimSpace(o.Validation) == "" {
		o.Validation = rustValidationIndexed
	}
	if strings.TrimSpace(o.Feedback) == "" {
		o.Feedback = rustFeedbackProfile
	}
	switch o.Visibility {
	case rustVisibilityMVCC, rustVisibilityMaterialized:
	default:
		return o, fmt.Errorf("invalid Rust SymbGraph visibility mode %q (want mvcc or materialized)", o.Visibility)
	}
	switch o.Validation {
	case rustValidationIndexed, rustValidationScan:
	default:
		return o, fmt.Errorf("invalid Rust SymbGraph validation mode %q (want indexed or scan)", o.Validation)
	}
	switch o.Feedback {
	case rustFeedbackProfile, rustFeedbackAllPairs:
	default:
		return o, fmt.Errorf("invalid Rust SymbGraph feedback mode %q (want profile or all-pairs)", o.Feedback)
	}
	return o, nil
}

func (o RustSymbGraphRunnerOptions) Variant() string {
	return o.Visibility + "+" + o.Validation + "+" + o.Feedback
}

func (o RustSymbGraphRunnerOptions) Scope() string {
	return "actual-wasmd-wasmvm-cosmos-sdk-symbgraph-rust-acg-ready-dag-launch-visible-" + o.Visibility + "-" + o.Validation + "-validate-" + o.Feedback + "-feedback-replay-no-ante-abci"
}

type RustSymbGraphDiagnostics struct {
	Variant string

	PlanNanos                         uint64
	PlanRequestBuildNanos             uint64
	PlanRequestMarshalNanos           uint64
	PlanCGORoundTripNanos             uint64
	PlanResponseUnmarshalNanos        uint64
	PlanRustDecodeNanos               uint64
	PlanResolveComponentsNanos        uint64
	PlanCandidateGraphNanos           uint64
	PlanSchedulerNanos                uint64
	PlanProjectionNanos               uint64
	PlanFeedbackPairsNanos            uint64
	PlanFinalizeNanos                 uint64
	PlanBridgeOtherNanos              uint64
	PreexecutionNanos                 uint64
	BranchCreateNanos                 uint64
	VisibilityNanos                   uint64
	SpecExecutionNanos                uint64
	DeltaCaptureNanos                 uint64
	MVCCPublishNanos                  uint64
	FeedbackBuildNanos                uint64
	ReconciliationNanos               uint64
	ValidationNanos                   uint64
	ReplayExecutionNanos              uint64
	RustFeedbackNanos                 uint64
	DependencyEdges                   int
	FeedbackPairs                     int
	PhysicalCandidateEdges            int
	LogicalCandidateEdges             int
	CompactCandidateGroups            int
	ParentDependenciesBeforeReduction int
	ParentDependenciesElidedReduction int
	DependencyReasons                 map[string]int
	DependencyPrimary                 map[string]int
	DependencyProvenance              map[string]int
	DependencyDecisions               map[string]int
	CriticalPath                      []int
	CriticalPathReasons               map[string]int
	CriticalPathProvenance            map[string]int
	CriticalPathDecisions             map[string]int
	CriticalPathCostByReason          map[string]uint64
	CandidateHard                     int
	CandidateSoft                     int
	CandidateLow                      int
	OrderedHard                       int
	OrderedSoft                       int
	OracleConflictEdges               int
	OracleCriticalPathTx              int
	OracleCriticalPathCost            uint64
	OracleDAGParallelism              float64
	OracleCriticalPath                []int
	SerializationGap                  float64
	InitialReady                      int
	MaxReady                          int
	ReadySamples                      uint64
	ReadySum                          uint64
	MaxActive                         int
	CriticalPathTx                    int
	CriticalPathCost                  uint64
	TotalEstimatedCost                uint64
	DAGParallelism                    float64
	WorkerUtilization                 float64
	WorkerIdleNanos                   uint64
	MVCCPointReads                    uint64
	MVCCVersionHits                   uint64
	MVCCBaseFallbacks                 uint64
	MVCCRangeReads                    uint64
	MVCCRangeOverlayKeys              uint64
	MVCCPublishes                     uint64
	MVCCPublishedKeys                 uint64
}

func (d RustSymbGraphDiagnostics) AverageReady() float64 {
	if d.ReadySamples == 0 {
		return 0
	}
	return float64(d.ReadySum) / float64(d.ReadySamples)
}

func rustCriticalPathDetail(count int, dependencies []rustPlanDependency, costs []uint32) (int, uint64, []int) {
	preds, _, _, err := rustDependencyTopology(count, dependencies)
	if err != nil || count == 0 {
		return 0, 0, nil
	}
	length := make([]int, count)
	cost := make([]uint64, count)
	previous := make([]int, count)
	for i := range previous {
		previous[i] = -1
	}
	bestTx, bestLength, bestCost := -1, 0, uint64(0)
	for tx := 0; tx < count; tx++ {
		selfCost := uint64(1)
		if tx < len(costs) && costs[tx] != 0 {
			selfCost = uint64(costs[tx])
		}
		length[tx] = 1
		cost[tx] = selfCost
		for _, predecessor := range preds[tx] {
			candidateLength := length[predecessor] + 1
			candidateCost := cost[predecessor] + selfCost
			if candidateCost > cost[tx] || (candidateCost == cost[tx] && candidateLength > length[tx]) {
				cost[tx] = candidateCost
				length[tx] = candidateLength
				previous[tx] = predecessor
			}
		}
		if cost[tx] > bestCost || (cost[tx] == bestCost && length[tx] > bestLength) {
			bestTx = tx
			bestCost = cost[tx]
			bestLength = length[tx]
		}
	}
	path := make([]int, 0, bestLength)
	for tx := bestTx; tx >= 0; tx = previous[tx] {
		path = append(path, tx)
		if previous[tx] < 0 {
			break
		}
	}
	for left, right := 0, len(path)-1; left < right; left, right = left+1, right-1 {
		path[left], path[right] = path[right], path[left]
	}
	return bestLength, bestCost, path
}

func rustCriticalPath(count int, dependencies []rustPlanDependency, costs []uint32) (int, uint64) {
	length, cost, _ := rustCriticalPathDetail(count, dependencies, costs)
	return length, cost
}

func rustDependencyReasonLookup(items []rustPlanDependencyReasons) map[[2]int][]string {
	out := make(map[[2]int][]string, len(items))
	for _, item := range items {
		if item.Predecessor < 0 || item.Successor < 0 || item.Predecessor == item.Successor {
			continue
		}
		key := [2]int{item.Predecessor, item.Successor}
		out[key] = append([]string(nil), item.Reasons...)
	}
	return out
}

func rustPrimaryDependencyReason(reasons []string) string {
	// Exact adapter resources are unavoidably hard. Static predicate-proven
	// relationships come next, then learned/adaptive hard edges, then soft-risk
	// scheduling choices. Projection is a preservation mechanism rather than a
	// semantic cause, so it is only primary when no semantic reason is present.
	priority := []string{"bank_resource", "symbolic_hard", "adaptive_hard", "soft_risk", "projection_hard"}
	for _, candidate := range priority {
		for _, reason := range reasons {
			if reason == candidate {
				return candidate
			}
		}
	}
	return "unknown"
}

func rustDependencyReasonDiagnostics(plan rustPlanResponse, criticalPath []int, costs []uint32) (map[string]int, map[string]int, map[string]int, map[string]uint64) {
	allReasons := map[string]int{}
	primary := map[string]int{}
	criticalReasons := map[string]int{}
	criticalCost := map[string]uint64{}
	lookup := rustDependencyReasonLookup(plan.DependencyReasons)
	if len(criticalPath) != 0 {
		root := criticalPath[0]
		rootCost := uint64(1)
		if root >= 0 && root < len(costs) && costs[root] != 0 {
			rootCost = uint64(costs[root])
		}
		criticalCost["root"] += rootCost
	}
	for _, dependency := range plan.Dependencies {
		reasons := lookup[[2]int{dependency.Predecessor, dependency.Successor}]
		if len(reasons) == 0 {
			reasons = []string{"unknown"}
		}
		for _, reason := range reasons {
			allReasons[reason]++
		}
		primary[rustPrimaryDependencyReason(reasons)]++
	}
	for i := 1; i < len(criticalPath); i++ {
		predecessor, tx := criticalPath[i-1], criticalPath[i]
		reasons := lookup[[2]int{predecessor, tx}]
		if len(reasons) == 0 {
			reasons = []string{"unknown"}
		}
		for _, reason := range reasons {
			criticalReasons[reason]++
		}
		primaryReason := rustPrimaryDependencyReason(reasons)
		selfCost := uint64(1)
		if tx >= 0 && tx < len(costs) && costs[tx] != 0 {
			selfCost = uint64(costs[tx])
		}
		criticalCost[primaryReason] += selfCost
	}
	return allReasons, primary, criticalReasons, criticalCost
}

func rustDependencyDimensionLookup(items []rustPlanDependencyReasons, dimension string) map[[2]int][]string {
	out := make(map[[2]int][]string, len(items))
	for _, item := range items {
		if item.Predecessor < 0 || item.Successor < 0 || item.Predecessor == item.Successor {
			continue
		}
		var values []string
		switch dimension {
		case "provenance":
			values = item.Provenance
		case "decision":
			values = item.Decisions
		default:
			values = item.Reasons
		}
		out[[2]int{item.Predecessor, item.Successor}] = append([]string(nil), values...)
	}
	return out
}

func rustDependencyDimensionDiagnostics(plan rustPlanResponse, criticalPath []int, dimension string) (map[string]int, map[string]int) {
	all := map[string]int{}
	critical := map[string]int{}
	lookup := rustDependencyDimensionLookup(plan.DependencyReasons, dimension)
	for _, dependency := range plan.Dependencies {
		values := lookup[[2]int{dependency.Predecessor, dependency.Successor}]
		if len(values) == 0 {
			values = []string{"unknown"}
		}
		for _, value := range values {
			all[value]++
		}
	}
	for i := 1; i < len(criticalPath); i++ {
		values := lookup[[2]int{criticalPath[i-1], criticalPath[i]}]
		if len(values) == 0 {
			values = []string{"unknown"}
		}
		for _, value := range values {
			critical[value]++
		}
	}
	return all, critical
}

func rustActualConflictOracle(trackers []*accessTracker, costs []uint32) (int, int, uint64, float64, []int) {
	dependencies := make([]rustPlanDependency, 0)
	for left := 0; left < len(trackers); left++ {
		if trackers[left] == nil {
			continue
		}
		for right := left + 1; right < len(trackers); right++ {
			if trackers[right] == nil || rustConflictKinds(trackers[left], trackers[right]) == 0 {
				continue
			}
			dependencies = append(dependencies, rustPlanDependency{Predecessor: left, Successor: right, Class: "hard"})
		}
	}
	length, criticalCost, path := rustCriticalPathDetail(len(trackers), dependencies, costs)
	totalCost := uint64(0)
	for _, cost := range costs {
		if cost == 0 {
			totalCost++
		} else {
			totalCost += uint64(cost)
		}
	}
	parallelism := 0.0
	if criticalCost != 0 {
		parallelism = float64(totalCost) / float64(criticalCost)
	}
	return len(dependencies), length, criticalCost, parallelism, path
}
