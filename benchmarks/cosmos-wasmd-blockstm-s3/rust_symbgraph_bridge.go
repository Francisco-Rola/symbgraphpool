package main

import (
	"encoding/json"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
	"unicode"
)

type rustSymbolicDocument struct {
	Family   string          `json:"family"`
	Document json.RawMessage `json:"document"`
}

type rustSymbolicProfileKey struct {
	Family     string
	Entrypoint string
}

type rustPlanningOverrides struct {
	EdgeMaterializationThreshold           *float64 `json:"edge_materialization_threshold,omitempty"`
	SoftThreshold                          *float64 `json:"soft_threshold,omitempty"`
	HardThreshold                          *float64 `json:"hard_threshold,omitempty"`
	RiskBudget                             *float64 `json:"risk_budget,omitempty"`
	ExplorationRate                        *float64 `json:"exploration_rate,omitempty"`
	ExplorationRiskBudget                  *float64 `json:"exploration_risk_budget,omitempty"`
	ExplorationMinUncertainty              *float64 `json:"exploration_min_uncertainty,omitempty"`
	ExplorationMaxTransactionsPerBlock     *int     `json:"exploration_max_transactions_per_block,omitempty"`
	IndependentObservationsBeforeSoftening *uint32  `json:"independent_observations_before_softening,omitempty"`
	SofteningMinConfidence                 *float64 `json:"softening_min_confidence,omitempty"`
}

type rustBridgeConfig struct {
	Documents             []rustSymbolicDocument              `json:"documents"`
	Planning              *rustPlanningOverrides              `json:"planning,omitempty"`
	DependencyDiagnostics bool                                `json:"dependency_diagnostics,omitempty"`
	Profiles              map[rustSymbolicProfileKey]struct{} `json:"-"`
}

type rustPlanComponent struct {
	Family                 string         `json:"family"`
	Instance               string         `json:"instance"`
	Entrypoint             string         `json:"entrypoint"`
	Bindings               map[string]any `json:"bindings"`
	EstimatedExecutionCost uint32         `json:"estimated_execution_cost"`
}

type rustPlanTransaction struct {
	TxID          uint64              `json:"tx_id"`
	Components    []rustPlanComponent `json:"components"`
	HardResources []string            `json:"hard_resources,omitempty"`
}

type rustPlanRequest struct {
	Epoch        uint64                `json:"epoch"`
	Transactions []rustPlanTransaction `json:"transactions"`
}

type rustPlanDependency struct {
	Predecessor int    `json:"predecessor"`
	Successor   int    `json:"successor"`
	Class       string `json:"class"`
}

type rustPlanDependencyReasons struct {
	Predecessor int      `json:"predecessor"`
	Successor   int      `json:"successor"`
	Reasons     []string `json:"reasons"`
	Provenance  []string `json:"provenance,omitempty"`
	Decisions   []string `json:"decisions,omitempty"`
}

type rustPlanningTimings struct {
	RustDecodeNanos        uint64 `json:"rust_decode_nanos"`
	ResolveComponentsNanos uint64 `json:"resolve_components_nanos"`
	CandidateGraphNanos    uint64 `json:"candidate_graph_nanos"`
	SchedulerNanos         uint64 `json:"scheduler_nanos"`
	ProjectionNanos        uint64 `json:"projection_nanos"`
	FeedbackPairsNanos     uint64 `json:"feedback_pairs_nanos"`
	FinalizeNanos          uint64 `json:"finalize_nanos"`
}

type rustCandidateDecisionCounts struct {
	Hard        int `json:"hard"`
	Soft        int `json:"soft"`
	Low         int `json:"low"`
	OrderedHard int `json:"ordered_hard"`
	OrderedSoft int `json:"ordered_soft"`
}

type rustBridgePlanTimings struct {
	RequestBuildNanos      uint64 `json:"-"`
	RequestMarshalNanos    uint64 `json:"-"`
	CGORoundTripNanos      uint64 `json:"-"`
	ResponseUnmarshalNanos uint64 `json:"-"`
}

type rustPlanningSnapshot struct {
	EdgeMaterializationThreshold           float64 `json:"edge_materialization_threshold"`
	SoftThreshold                          float64 `json:"soft_threshold"`
	HardThreshold                          float64 `json:"hard_threshold"`
	RiskBudget                             float64 `json:"risk_budget"`
	ExplorationRate                        float64 `json:"exploration_rate"`
	ExplorationRiskBudget                  float64 `json:"exploration_risk_budget"`
	ExplorationMinUncertainty              float64 `json:"exploration_min_uncertainty"`
	ExplorationMaxTransactionsPerBlock     int     `json:"exploration_max_transactions_per_block"`
	IndependentObservationsBeforeSoftening uint32  `json:"independent_observations_before_softening"`
	SofteningMinConfidence                 float64 `json:"softening_min_confidence"`
}

type rustPlanPair struct {
	Left  int `json:"left"`
	Right int `json:"right"`
}

type rustPlanResponse struct {
	TransactionCount                    int                         `json:"transaction_count"`
	ComponentCount                      int                         `json:"component_count"`
	CandidateEdges                      int                         `json:"candidate_edges"`
	LogicalCandidateEdges               int                         `json:"logical_candidate_edges"`
	CompactCandidateGroups              int                         `json:"compact_candidate_groups"`
	PreReductionDependencies            int                         `json:"pre_reduction_dependencies"`
	ParentDependenciesBeforeReduction   int                         `json:"parent_dependencies_before_reduction"`
	ParentDependenciesElidedByReduction int                         `json:"parent_dependencies_elided_by_reduction"`
	Planning                            *rustPlanningSnapshot       `json:"planning,omitempty"`
	PlanningTimings                     *rustPlanningTimings        `json:"planning_timings,omitempty"`
	CandidateDecisions                  rustCandidateDecisionCounts `json:"candidate_decisions"`
	Dependencies                        []rustPlanDependency        `json:"dependencies"`
	DependencyReasons                   []rustPlanDependencyReasons `json:"dependency_reasons"`
	FeedbackPairs                       []rustPlanPair              `json:"feedback_pairs"`
	Levels                              [][]int                     `json:"levels"`
	BridgeTimings                       rustBridgePlanTimings       `json:"-"`
}

type rustPairObservation struct {
	Left          int    `json:"left"`
	Right         int    `json:"right"`
	ConflictKinds uint8  `json:"conflict_kinds"`
	Conflict      bool   `json:"conflict"`
	Source        string `json:"source"`
}

type rustReplayAttribution struct {
	Predecessor            int    `json:"predecessor"`
	Transaction            int    `json:"transaction"`
	ConflictKinds          uint8  `json:"conflict_kinds"`
	ReplayCostNanos        uint64 `json:"replay_cost_nanos"`
	InvalidatedDescendants uint32 `json:"invalidated_descendants"`
}

type rustSerializationObservation struct {
	Predecessor             int    `json:"predecessor"`
	Transaction             int    `json:"transaction"`
	MarginalReadyDelayNanos uint64 `json:"marginal_ready_delay_nanos"`
}

type rustEconomicsObservation struct {
	SerialServiceNanos uint64 `json:"serial_service_nanos"`
	PreConsensusNanos  uint64 `json:"pre_consensus_nanos"`
	PostConsensusNanos uint64 `json:"post_consensus_nanos"`
	TransactionCount   int    `json:"transaction_count"`
}

type rustFeedbackRequest struct {
	Epoch              uint64                         `json:"epoch"`
	Observations       []rustPairObservation          `json:"observations,omitempty"`
	ReplayAttributions []rustReplayAttribution        `json:"replay_attributions,omitempty"`
	Serialization      []rustSerializationObservation `json:"serialization,omitempty"`
	Economics          rustEconomicsObservation       `json:"economics"`
}

type rustFeedbackResponse struct {
	AppliedObservations          int  `json:"applied_observations"`
	PositiveObservations         int  `json:"positive_observations"`
	NegativeObservations         int  `json:"negative_observations"`
	UnattributedRuntimeConflicts int  `json:"unattributed_runtime_conflicts"`
	ReplayAttributionsApplied    int  `json:"replay_attributions_applied"`
	SerializationObservations    int  `json:"serialization_observations_applied"`
	RegimeEvidenceDecayed        bool `json:"regime_evidence_decayed"`
}

// rustSchedulerFFI is implemented by the cgo file when built with -tags acg_rust.
// The !acg_rust implementation returns an actionable build error while keeping
// ordinary Go-only unit tests buildable.
type rustSchedulerFFI interface {
	plan(rustPlanRequest) (rustPlanResponse, error)
	feedback(rustFeedbackRequest) (rustFeedbackResponse, error)
	close()
}

func loadRustBridgeConfig(repoRoot, symbolicDir string) (rustBridgeConfig, error) {
	if strings.TrimSpace(symbolicDir) == "" {
		symbolicDir = "benchmarks/symbolic/native-s3"
	}
	dir := resolveRepoPath(repoRoot, symbolicDir)
	entries, err := os.ReadDir(dir)
	if err != nil {
		return rustBridgeConfig{}, fmt.Errorf("read Rust symbolic directory %s: %w", dir, err)
	}
	var docs []rustSymbolicDocument
	profiles := make(map[rustSymbolicProfileKey]struct{})
	for _, entry := range entries {
		if entry.IsDir() || filepath.Ext(entry.Name()) != ".json" || isSymbolicBundleMetadata(entry.Name()) {
			continue
		}
		path := filepath.Join(dir, entry.Name())
		bz, err := os.ReadFile(path)
		if err != nil {
			return rustBridgeConfig{}, err
		}
		var header struct {
			Contract string `json:"contract"`
			Profiles []struct {
				Entrypoint string `json:"entrypoint"`
			} `json:"profiles"`
		}
		if err := json.Unmarshal(bz, &header); err != nil {
			return rustBridgeConfig{}, fmt.Errorf("parse symbolic profile header %s: %w", path, err)
		}
		if strings.TrimSpace(header.Contract) == "" {
			return rustBridgeConfig{}, fmt.Errorf("symbolic profile %s has empty contract", path)
		}
		for _, profile := range header.Profiles {
			entrypoint := strings.TrimSpace(profile.Entrypoint)
			if entrypoint == "" {
				continue
			}
			profiles[rustSymbolicProfileKey{Family: header.Contract, Entrypoint: entrypoint}] = struct{}{}
		}
		docs = append(docs, rustSymbolicDocument{Family: header.Contract, Document: append(json.RawMessage(nil), bz...)})
	}
	sort.Slice(docs, func(i, j int) bool { return docs[i].Family < docs[j].Family })
	if len(docs) == 0 {
		return rustBridgeConfig{}, fmt.Errorf("no symbolic JSON profiles found in %s", dir)
	}
	return rustBridgeConfig{Documents: docs, Profiles: profiles}, nil
}

func (c rustBridgeConfig) hasProfile(family, entrypoint string) bool {
	_, ok := c.Profiles[rustSymbolicProfileKey{Family: family, Entrypoint: entrypoint}]
	return ok
}

func canonicalVariant(value string) string {
	var out strings.Builder
	upperNext := true
	for _, r := range value {
		if r == '_' || r == '-' || unicode.IsSpace(r) {
			upperNext = true
			continue
		}
		if upperNext {
			out.WriteRune(unicode.ToUpper(r))
			upperNext = false
		} else {
			out.WriteRune(r)
		}
	}
	return out.String()
}

func rustCallEntrypoint(call CallSpec) (string, map[string]any, bool) {
	if call.Kind != "execute" && call.Kind != "query" {
		return "", nil, false
	}
	action, payload := messageActionAndPayload(call.Msg, call.Kind)
	if strings.TrimSpace(action) == "" {
		return "", nil, false
	}
	if payload == nil {
		payload = map[string]any{}
	}
	return call.Kind + "::" + canonicalVariant(action), payload, true
}

func cloneMap(in map[string]any) map[string]any {
	out := make(map[string]any, len(in)+2)
	for k, v := range in {
		out[k] = v
	}
	return out
}

func rustBindings(block ExecutionBlock, txIndex int, call CallSpec, payload map[string]any) map[string]any {
	root := cloneMap(payload)
	info := map[string]any{}
	if call.Sender != nil {
		info["sender"] = *call.Sender
	}
	if len(call.Funds) != 0 {
		info["funds"] = call.Funds
	}
	if len(info) != 0 {
		root["info"] = info
	}
	root["env"] = map[string]any{
		"block": map[string]any{
			"height":     block.BlockNumber,
			"time_nanos": block.Timestamp * 1_000_000_000,
			"chain_id":   chainID,
		},
		"transaction_index": txIndex,
	}
	return root
}

func rustHardResources(tx ExecutionTx) []string {
	set := map[string]struct{}{}
	add := func(address, denom string) {
		if address == "" || denom == "" {
			return
		}
		set["bank:"+address+":"+denom] = struct{}{}
	}
	for _, call := range tx.Calls {
		switch call.Kind {
		case "bank_send":
			for _, coin := range call.Coins {
				if call.From != nil {
					add(*call.From, coin.Denom)
				}
				if call.To != nil {
					add(*call.To, coin.Denom)
				}
			}
		case "execute":
			for _, coin := range call.Funds {
				if call.Sender != nil {
					add(*call.Sender, coin.Denom)
				}
				if call.InstanceID != nil {
					add(*call.InstanceID, coin.Denom)
				}
			}
		}
	}
	out := make([]string, 0, len(set))
	for resource := range set {
		out = append(out, resource)
	}
	sort.Strings(out)
	return out
}

func boundedCost(value uint64) uint32 {
	if value == 0 {
		return 1
	}
	if value > math.MaxUint32 {
		return math.MaxUint32
	}
	return uint32(value)
}

func distributeComponentCost(total uint32, count int) []uint32 {
	if count <= 0 {
		return nil
	}
	if total == 0 {
		total = 1
	}
	base := total / uint32(count)
	rem := total % uint32(count)
	out := make([]uint32, count)
	for i := range out {
		out[i] = base
		if uint32(i) < rem {
			out[i]++
		}
	}
	return out
}

func buildRustPlanRequest(block ExecutionBlock, estimatedCosts []uint32, profiles map[rustSymbolicProfileKey]struct{}) (rustPlanRequest, error) {
	transactions := make([]rustPlanTransaction, len(block.Transactions))
	for i, tx := range block.Transactions {
		componentCalls := make([]struct {
			call       CallSpec
			entrypoint string
			payload    map[string]any
		}, 0, len(tx.Calls))
		for _, call := range tx.Calls {
			entrypoint, payload, ok := rustCallEntrypoint(call)
			if !ok || call.Family == nil || call.InstanceID == nil {
				continue
			}
			if _, ok := profiles[rustSymbolicProfileKey{Family: *call.Family, Entrypoint: entrypoint}]; !ok {
				// Match the established S3 symbolic predictor semantics: an entrypoint
				// absent from the checked-in symbolic corpus has no predicted accesses.
				// Do not fabricate an ACG profile or alias it to a different operation.
				// The call still executes normally in Wasmd and concrete canonical
				// validation/replay remains the correctness authority.
				continue
			}
			componentCalls = append(componentCalls, struct {
				call       CallSpec
				entrypoint string
				payload    map[string]any
			}{call: call, entrypoint: entrypoint, payload: payload})
		}
		parentCost := uint32(max(1, len(tx.Calls)))
		if i < len(estimatedCosts) && estimatedCosts[i] != 0 {
			parentCost = estimatedCosts[i]
		}
		costs := distributeComponentCost(parentCost, len(componentCalls))
		components := make([]rustPlanComponent, 0, len(componentCalls))
		for j, item := range componentCalls {
			components = append(components, rustPlanComponent{
				Family:                 *item.call.Family,
				Instance:               *item.call.InstanceID,
				Entrypoint:             item.entrypoint,
				Bindings:               rustBindings(block, i, item.call, item.payload),
				EstimatedExecutionCost: costs[j],
			})
		}
		transactions[i] = rustPlanTransaction{
			TxID:          uint64(tx.TxIndex),
			Components:    components,
			HardResources: rustHardResources(tx),
		}
	}
	return rustPlanRequest{Epoch: block.BlockNumber, Transactions: transactions}, nil
}

type RustSymbGraphBridge struct {
	ffi      rustSchedulerFFI
	profiles map[rustSymbolicProfileKey]struct{}
}

func NewRustSymbGraphBridge(repoRoot, symbolicDir string) (*RustSymbGraphBridge, error) {
	return NewRustSymbGraphBridgeWithPlanningAndDiagnostics(repoRoot, symbolicDir, nil, false)
}

func NewRustSymbGraphBridgeWithPlanning(repoRoot, symbolicDir string, planning *rustPlanningOverrides) (*RustSymbGraphBridge, error) {
	return NewRustSymbGraphBridgeWithPlanningAndDiagnostics(repoRoot, symbolicDir, planning, false)
}

func NewRustSymbGraphBridgeWithPlanningAndDiagnostics(repoRoot, symbolicDir string, planning *rustPlanningOverrides, dependencyDiagnostics bool) (*RustSymbGraphBridge, error) {
	config, err := loadRustBridgeConfig(repoRoot, symbolicDir)
	if err != nil {
		return nil, err
	}
	config.Planning = planning
	config.DependencyDiagnostics = dependencyDiagnostics
	ffi, err := newRustSchedulerFFI(config)
	if err != nil {
		return nil, err
	}
	return &RustSymbGraphBridge{ffi: ffi, profiles: config.Profiles}, nil
}

func (b *RustSymbGraphBridge) Close() {
	if b != nil && b.ffi != nil {
		b.ffi.close()
		b.ffi = nil
	}
}

func (b *RustSymbGraphBridge) Plan(block ExecutionBlock, estimatedCosts []uint32) (rustPlanResponse, error) {
	if b == nil || b.ffi == nil {
		return rustPlanResponse{}, fmt.Errorf("Rust SymbGraph bridge is closed")
	}
	buildStarted := time.Now()
	request, err := buildRustPlanRequest(block, estimatedCosts, b.profiles)
	if err != nil {
		return rustPlanResponse{}, err
	}
	buildNanos := uint64(time.Since(buildStarted).Nanoseconds())
	response, err := b.ffi.plan(request)
	if err != nil {
		return rustPlanResponse{}, err
	}
	response.BridgeTimings.RequestBuildNanos = buildNanos
	return response, nil
}

func (b *RustSymbGraphBridge) Feedback(request rustFeedbackRequest) (rustFeedbackResponse, error) {
	if b == nil || b.ffi == nil {
		return rustFeedbackResponse{}, fmt.Errorf("Rust SymbGraph bridge is closed")
	}
	return b.ffi.feedback(request)
}
