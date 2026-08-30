package main

import (
	"bufio"
	"bytes"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

// exactEthereumTraceIndex is evaluation-only hindsight information from the
// frozen custom-JS SLOAD/SSTORE trace. It deliberately contains source EVM
// resources rather than concrete Wasmd accesses: this row asks how Rust-ACG
// behaves if symbolic analysis predicts the source workload perfectly.
type exactEthereumTraceIndex struct {
	byHash map[string]exactEthereumTraceAccess
}

type exactEthereumTraceAccess struct {
	reads  []string
	writes []string
}

type exactEthereumTraceFile struct {
	TxHash string `json:"tx_hash"`
	Result struct {
		Reads  []string `json:"reads"`
		Writes []string `json:"writes"`
	} `json:"result"`
}

type exactTracePlanDiagnostics struct {
	SourceReads                  uint64
	SourceWrites                 uint64
	MissingTraces                int
	DependencyEdges              int
	AdapterHardEdges             int
	TranslationCompensationEdges int
	MissingBarrierEdges          int
}

// exactNativeTranslationIndex is a frozen, evaluation-only audit of the native
// translation's logical storage/bank accesses. It is not used as the oracle's
// primary access source: exact Ethereum SLOAD/SSTORE remains authoritative.
// The index contributes only RAW edges that exist in the translated native
// workload but are absent from the source trace, compensating for semantic
// aliasing/read-modify-write introduced by the EVM->CosmWasm translation.
type exactNativeTranslationIndex struct {
	byBlock map[uint64]exactNativeTranslationBlock
}

type exactNativeTranslationBlock struct {
	txs map[int]exactNativeTranslationTx
}

type exactNativeTranslationTx struct {
	hash   string
	reads  []exactNativePoint
	ranges []exactNativeRange
	writes []exactNativePoint
}

type exactNativePoint struct {
	namespace string
	key       string
}

type exactNativeRange struct {
	namespace string
	start     []byte
	end       []byte
}

type exactNativeAccessFileBlock struct {
	BlockNumber  uint64 `json:"block_number"`
	Transactions []struct {
		TxIndex         int    `json:"tx_index"`
		TxHash          string `json:"tx_hash"`
		SourceFailed    bool   `json:"source_failed"`
		ExecutionStatus string `json:"execution_status"`
		Accesses        []struct {
			Kind        string  `json:"kind"`
			Contract    string  `json:"contract"`
			KeyHex      string  `json:"key_hex"`
			RangeEndHex *string `json:"range_end_hex"`
			Reverted    bool    `json:"reverted"`
		} `json:"accesses"`
	} `json:"transactions"`
}

func decodeNativeHex(value string) ([]byte, error) {
	value = strings.TrimPrefix(strings.TrimSpace(value), "0x")
	if value == "" {
		return nil, nil
	}
	decoded, err := hex.DecodeString(value)
	if err != nil {
		return nil, err
	}
	return decoded, nil
}

func loadExactNativeTranslationIndex(path string) (*exactNativeTranslationIndex, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, fmt.Errorf("open frozen native translation accesses %s: %w", path, err)
	}
	defer f.Close()
	index := &exactNativeTranslationIndex{byBlock: make(map[uint64]exactNativeTranslationBlock, 128)}
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 64*1024), 64*1024*1024)
	for scanner.Scan() {
		if strings.TrimSpace(scanner.Text()) == "" {
			continue
		}
		var input exactNativeAccessFileBlock
		if err := json.Unmarshal(scanner.Bytes(), &input); err != nil {
			return nil, fmt.Errorf("decode frozen native translation accesses %s: %w", path, err)
		}
		if _, exists := index.byBlock[input.BlockNumber]; exists {
			return nil, fmt.Errorf("duplicate native translation access block %d", input.BlockNumber)
		}
		block := exactNativeTranslationBlock{txs: make(map[int]exactNativeTranslationTx, len(input.Transactions))}
		for _, tx := range input.Transactions {
			parsed := exactNativeTranslationTx{hash: normalizeEthereumTxHash(tx.TxHash)}
			discardWrites := tx.SourceFailed || strings.EqualFold(tx.ExecutionStatus, "reverted")
			for _, access := range tx.Accesses {
				key, err := decodeNativeHex(access.KeyHex)
				if err != nil {
					return nil, fmt.Errorf("block %d tx %d decode native key: %w", input.BlockNumber, tx.TxIndex, err)
				}
				namespace := "storage:" + strings.ToLower(strings.TrimSpace(access.Contract))
				if strings.HasPrefix(access.Kind, "bank_") {
					namespace = "bank"
				}
				point := exactNativePoint{namespace: namespace, key: string(key)}
				switch access.Kind {
				case "storage_read", "bank_read":
					parsed.reads = append(parsed.reads, point)
				case "storage_scan":
					var end []byte
					if access.RangeEndHex != nil {
						end, err = decodeNativeHex(*access.RangeEndHex)
						if err != nil {
							return nil, fmt.Errorf("block %d tx %d decode native range end: %w", input.BlockNumber, tx.TxIndex, err)
						}
					}
					parsed.ranges = append(parsed.ranges, exactNativeRange{namespace: namespace, start: key, end: end})
				case "storage_write", "storage_remove", "bank_write":
					if !discardWrites && !access.Reverted {
						parsed.writes = append(parsed.writes, point)
					}
				}
			}
			block.txs[tx.TxIndex] = parsed
		}
		index.byBlock[input.BlockNumber] = block
	}
	if err := scanner.Err(); err != nil {
		return nil, err
	}
	if len(index.byBlock) == 0 {
		return nil, fmt.Errorf("no native translation access blocks found in %s", path)
	}
	return index, nil
}

func nativePointInRange(point exactNativePoint, read exactNativeRange) bool {
	if point.namespace != read.namespace {
		return false
	}
	key := []byte(point.key)
	if read.start != nil && bytes.Compare(key, read.start) < 0 {
		return false
	}
	return read.end == nil || bytes.Compare(key, read.end) < 0
}

func (index *exactNativeTranslationIndex) addRAWCompensationEdges(block ExecutionBlock, edges map[[2]int]struct{}) (int, error) {
	if index == nil {
		return 0, fmt.Errorf("nil frozen native translation access index")
	}
	nativeBlock, ok := index.byBlock[block.BlockNumber]
	if !ok {
		return 0, fmt.Errorf("missing frozen native translation access block %d", block.BlockNumber)
	}
	lastWriter := make(map[exactNativePoint]int)
	added := 0
	for position, tx := range block.Transactions {
		nativeTx, ok := nativeBlock.txs[tx.TxIndex]
		if !ok {
			return 0, fmt.Errorf("missing frozen native translation accesses block=%d tx=%d", block.BlockNumber, tx.TxIndex)
		}
		if nativeTx.hash != "" && nativeTx.hash != normalizeEthereumTxHash(tx.TxHash) {
			return 0, fmt.Errorf("native translation tx hash mismatch block=%d tx=%d got=%s want=%s", block.BlockNumber, tx.TxIndex, nativeTx.hash, normalizeEthereumTxHash(tx.TxHash))
		}
		for _, read := range nativeTx.reads {
			if writer, exists := lastWriter[read]; exists && addExactOracleDependency(edges, writer, position) {
				added++
			}
		}
		for _, read := range nativeTx.ranges {
			for point, writer := range lastWriter {
				if nativePointInRange(point, read) && addExactOracleDependency(edges, writer, position) {
					added++
				}
			}
		}
		for _, write := range nativeTx.writes {
			lastWriter[write] = position
		}
	}
	return added, nil
}

func normalizeEthereumTxHash(hash string) string {
	return strings.ToLower(strings.TrimSpace(hash))
}

func loadExactEthereumTraceIndex(traceDir string) (*exactEthereumTraceIndex, error) {
	index := &exactEthereumTraceIndex{byHash: make(map[string]exactEthereumTraceAccess, 14000)}
	err := filepath.WalkDir(traceDir, func(path string, entry os.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if entry.IsDir() || filepath.Ext(entry.Name()) != ".json" {
			return nil
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		var trace exactEthereumTraceFile
		if err := json.Unmarshal(data, &trace); err != nil {
			return fmt.Errorf("decode exact Ethereum trace %s: %w", path, err)
		}
		hash := normalizeEthereumTxHash(trace.TxHash)
		if hash == "" {
			return fmt.Errorf("exact Ethereum trace %s has empty tx_hash", path)
		}
		if _, exists := index.byHash[hash]; exists {
			return fmt.Errorf("duplicate exact Ethereum trace tx_hash=%s", hash)
		}
		index.byHash[hash] = exactEthereumTraceAccess{
			reads:  append([]string(nil), trace.Result.Reads...),
			writes: append([]string(nil), trace.Result.Writes...),
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	if len(index.byHash) == 0 {
		return nil, fmt.Errorf("no exact Ethereum traces found in %s", traceDir)
	}
	return index, nil
}

type exactResourceOrderState struct {
	lastWriter int
}

func addExactOracleDependency(edges map[[2]int]struct{}, predecessor, successor int) bool {
	if predecessor < 0 || successor < 0 || predecessor >= successor {
		return false
	}
	key := [2]int{predecessor, successor}
	if _, exists := edges[key]; exists {
		return false
	}
	edges[key] = struct{}{}
	return true
}

// buildExactTraceACGPlan converts perfect source SLOAD/SSTORE information into
// the minimal hard dependencies required for zero replay under Rust-ACG's own
// launch-visibility + canonical-validation semantics. A later read waits for
// the most recent earlier committed writer of that key (RAW). Pure WAW and WAR
// relationships stay parallel: the runner applies deltas in canonical order,
// and neither relationship can invalidate a speculative read by itself. This
// is intentionally less restrictive than a generic conflict-free DAG because
// the goal is the upper bound of the existing ACG machinery with perfect
// symbolic accesses, not the upper bound of a different scheduler.
//
// Adapter-level bank/funds resources are merged because they are real Wasmd
// state outside the Ethereum storage trace. A source trace that is absent is a
// conservative serial barrier. The frozen corpus currently allows a tiny
// number of missing traces; making them barriers keeps the oracle fail-safe
// while making the incompleteness explicit in the record.
func (index *exactEthereumTraceIndex) buildExactTraceACGPlan(block ExecutionBlock) (rustPlanResponse, exactTracePlanDiagnostics, error) {
	return index.buildExactTraceACGPlanWithTranslation(block, nil)
}

func (index *exactEthereumTraceIndex) buildExactTraceACGPlanWithTranslation(block ExecutionBlock, nativeIndex *exactNativeTranslationIndex) (rustPlanResponse, exactTracePlanDiagnostics, error) {
	if index == nil {
		return rustPlanResponse{}, exactTracePlanDiagnostics{}, fmt.Errorf("nil exact Ethereum trace index")
	}
	edges := make(map[[2]int]struct{})
	resources := make(map[string]*exactResourceOrderState)
	lastAdapterUser := make(map[string]int)
	missing := make([]int, 0, 2)
	diag := exactTracePlanDiagnostics{}

	for txIndex, tx := range block.Transactions {
		trace, ok := index.byHash[normalizeEthereumTxHash(tx.TxHash)]
		if !ok {
			diag.MissingTraces++
			missing = append(missing, txIndex)
		} else {
			readSet := make(map[string]struct{}, len(trace.reads)+len(trace.writes))
			writeSet := make(map[string]struct{}, len(trace.writes))
			for _, key := range trace.reads {
				if key = strings.TrimSpace(key); key != "" {
					readSet[key] = struct{}{}
				}
			}
			for _, key := range trace.writes {
				if key = strings.TrimSpace(key); key != "" {
					if tx.SourceFailed {
						// Reverted EVM writes are observable accesses but do not alter
						// canonical state. Treat them as reads for scheduling purposes,
						// matching the native translation's discarded-write semantics.
						readSet[key] = struct{}{}
					} else {
						writeSet[key] = struct{}{}
					}
				}
			}
			diag.SourceReads += uint64(len(readSet))
			diag.SourceWrites += uint64(len(writeSet))

			// Reads are the only operation that can become stale in the
			// canonical validator. Make every exact read wait for the latest
			// earlier writer, including read-modify-write transactions.
			for key := range readSet {
				state := resources[key]
				if state == nil {
					state = &exactResourceOrderState{lastWriter: -1}
					resources[key] = state
				}
				if state.lastWriter >= 0 {
					addExactOracleDependency(edges, state.lastWriter, txIndex)
				}
			}

			// A write only becomes the version future reads must observe. Pure
			// WAW/WAR relationships do not need a scheduling edge because final
			// deltas are committed in canonical transaction order.
			for key := range writeSet {
				state := resources[key]
				if state == nil {
					state = &exactResourceOrderState{lastWriter: -1}
					resources[key] = state
				}
				state.lastWriter = txIndex
			}
		}

		for _, resource := range rustHardResources(tx) {
			if predecessor, exists := lastAdapterUser[resource]; exists {
				if addExactOracleDependency(edges, predecessor, txIndex) {
					diag.AdapterHardEdges++
				}
			}
			lastAdapterUser[resource] = txIndex
		}
	}

	if nativeIndex != nil {
		added, err := nativeIndex.addRAWCompensationEdges(block, edges)
		if err != nil {
			return rustPlanResponse{}, exactTracePlanDiagnostics{}, err
		}
		diag.TranslationCompensationEdges = added
	}

	for _, barrier := range missing {
		for predecessor := 0; predecessor < barrier; predecessor++ {
			if addExactOracleDependency(edges, predecessor, barrier) {
				diag.MissingBarrierEdges++
			}
		}
		for successor := barrier + 1; successor < len(block.Transactions); successor++ {
			if addExactOracleDependency(edges, barrier, successor) {
				diag.MissingBarrierEdges++
			}
		}
	}

	dependencies := make([]rustPlanDependency, 0, len(edges))
	for pair := range edges {
		dependencies = append(dependencies, rustPlanDependency{
			Predecessor: pair[0],
			Successor:   pair[1],
			Class:       "hard",
		})
	}
	sort.Slice(dependencies, func(i, j int) bool {
		if dependencies[i].Predecessor != dependencies[j].Predecessor {
			return dependencies[i].Predecessor < dependencies[j].Predecessor
		}
		return dependencies[i].Successor < dependencies[j].Successor
	})
	diag.DependencyEdges = len(dependencies)

	return rustPlanResponse{
		TransactionCount:                  len(block.Transactions),
		CandidateEdges:                    len(dependencies),
		LogicalCandidateEdges:             len(dependencies),
		PreReductionDependencies:          len(dependencies),
		ParentDependenciesBeforeReduction: len(dependencies),
		CandidateDecisions: rustCandidateDecisionCounts{
			Hard:        len(dependencies),
			OrderedHard: len(dependencies),
		},
		Dependencies: dependencies,
	}, diag, nil
}
