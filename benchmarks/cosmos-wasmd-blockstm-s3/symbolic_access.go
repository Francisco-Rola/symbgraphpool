package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"unicode"
)

type symbolicProfileKey struct {
	family     string
	entrypoint string
}

type symbolicDocument struct {
	Contract string            `json:"contract"`
	Profiles []symbolicProfile `json:"profiles"`
}

type symbolicProfile struct {
	Entrypoint string           `json:"entrypoint"`
	Accesses   []symbolicAccess `json:"accesses"`
}

type symbolicAccess struct {
	Kind     string      `json:"kind"`
	Resource string      `json:"resource"`
	Key      symbolicKey `json:"key"`
}

type symbolicKey struct {
	SemanticName *string             `json:"semantic_name"`
	DependsOn    *symbolicDependency `json:"depends_on"`
}

type symbolicDependency struct {
	OriginInput *string `json:"origin_input"`
}

type predictedLocation struct {
	scope    string
	resource string
	key      string
}

type predictedAccess struct {
	location predictedLocation
	write    bool
}

type SymbolicPredictor struct {
	profiles      map[symbolicProfileKey][]symbolicAccess
	documentCount int
	profileCount  int
	sourceDir     string
}

func normalizeEntrypoint(value string) string {
	var b strings.Builder
	for _, r := range value {
		if unicode.IsLetter(r) || unicode.IsDigit(r) {
			b.WriteRune(unicode.ToLower(r))
		}
	}
	return b.String()
}

func resolveRepoPath(repoRoot, path string) string {
	if filepath.IsAbs(path) {
		return path
	}
	return filepath.Join(repoRoot, path)
}

func isSymbolicBundleMetadata(name string) bool {
	// Workload-local symbolic bundles carry a manifest alongside the actual
	// contract profile JSON files. It describes copied profiles and checksums;
	// it is not itself a symbolic contract document.
	return name == "manifest.json"
}

func loadSymbolicPredictor(repoRoot, symbolicDir string) (*SymbolicPredictor, error) {
	if strings.TrimSpace(symbolicDir) == "" {
		symbolicDir = "benchmarks/symbolic/native-s3"
	}
	dir := resolveRepoPath(repoRoot, symbolicDir)
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, fmt.Errorf("read S3 symbolic directory %s: %w", dir, err)
	}

	p := &SymbolicPredictor{
		profiles:  map[symbolicProfileKey][]symbolicAccess{},
		sourceDir: symbolicDir,
	}
	for _, entry := range entries {
		if entry.IsDir() || filepath.Ext(entry.Name()) != ".json" || isSymbolicBundleMetadata(entry.Name()) {
			continue
		}
		path := filepath.Join(dir, entry.Name())
		data, err := os.ReadFile(path)
		if err != nil {
			return nil, err
		}
		var doc symbolicDocument
		if err := json.Unmarshal(data, &doc); err != nil {
			return nil, fmt.Errorf("parse symbolic profile %s: %w", path, err)
		}
		if strings.TrimSpace(doc.Contract) == "" {
			return nil, fmt.Errorf("symbolic profile %s has empty contract", path)
		}
		p.documentCount++
		for _, profile := range doc.Profiles {
			key := symbolicProfileKey{
				family:     doc.Contract,
				entrypoint: normalizeEntrypoint(profile.Entrypoint),
			}
			p.profiles[key] = append([]symbolicAccess(nil), profile.Accesses...)
			p.profileCount++
		}
	}
	if p.documentCount == 0 || p.profileCount == 0 {
		return nil, fmt.Errorf("S3 symbolic directory %s contained no contract profiles", dir)
	}
	return p, nil
}

func messageActionAndPayload(msg any, fallback string) (string, map[string]any) {
	obj, ok := msg.(map[string]any)
	if !ok || len(obj) == 0 {
		return fallback, nil
	}
	keys := make([]string, 0, len(obj))
	for key := range obj {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	action := keys[0]
	payload, _ := obj[action].(map[string]any)
	return action, payload
}

func symbolicAtom(name string, call CallSpec, payload map[string]any) (string, bool) {
	name = strings.TrimSpace(name)
	if name == "info.sender" {
		if call.Sender == nil {
			return "", false
		}
		return *call.Sender, true
	}
	if payload == nil {
		return "", false
	}
	value, ok := payload[name]
	if !ok {
		return "", false
	}
	switch x := value.(type) {
	case string:
		return x, true
	case bool:
		if x {
			return "true", true
		}
		return "false", true
	case float64:
		bz, _ := json.Marshal(x)
		return string(bz), true
	case json.Number:
		return x.String(), true
	default:
		bz, err := json.Marshal(value)
		if err != nil {
			return "", false
		}
		return string(bz), true
	}
}

func resolveSymbolicKey(call CallSpec, key symbolicKey) string {
	if key.DependsOn == nil {
		if key.SemanticName == nil || *key.SemanticName == "singleton" {
			return "singleton"
		}
		return "*"
	}
	if key.DependsOn.OriginInput == nil {
		return "*"
	}
	expr := strings.TrimSpace(*key.DependsOn.OriginInput)
	_, payload := messageActionAndPayload(call.Msg, call.Kind)
	if strings.HasPrefix(expr, "(") && strings.HasSuffix(expr, ")") {
		inner := expr[1 : len(expr)-1]
		parts := strings.Split(inner, ",")
		values := make([]string, 0, len(parts))
		for _, part := range parts {
			value, ok := symbolicAtom(part, call, payload)
			if !ok {
				return "*"
			}
			values = append(values, value)
		}
		return strings.Join(values, "|")
	}
	if value, ok := symbolicAtom(expr, call, payload); ok {
		return value
	}
	return "*"
}

func (p *SymbolicPredictor) predictTx(tx ExecutionTx) staticFootprint {
	fp := newStaticFootprint()
	for _, call := range tx.Calls {
		if call.Kind == "bank_send" {
			for _, coin := range call.Coins {
				for _, address := range []*string{call.From, call.To} {
					if address == nil {
						continue
					}
					fp.accesses = append(fp.accesses, predictedAccess{
						location: predictedLocation{scope: "bank", resource: *address, key: coin.Denom},
						write:    true,
					})
				}
			}
			continue
		}

		if call.Kind == "execute" {
			for _, coin := range call.Funds {
				if call.Sender != nil {
					fp.accesses = append(fp.accesses, predictedAccess{
						location: predictedLocation{scope: "bank", resource: *call.Sender, key: coin.Denom},
						write:    true,
					})
				}
				if call.InstanceID != nil {
					fp.accesses = append(fp.accesses, predictedAccess{
						location: predictedLocation{scope: "bank", resource: *call.InstanceID, key: coin.Denom},
						write:    true,
					})
				}
			}
		}

		if call.Family == nil || call.InstanceID == nil {
			continue
		}
		action, _ := messageActionAndPayload(call.Msg, call.Kind)
		entrypoint := normalizeEntrypoint(call.Kind + "::" + action)
		accesses := p.profiles[symbolicProfileKey{family: *call.Family, entrypoint: entrypoint}]
		for _, access := range accesses {
			fp.accesses = append(fp.accesses, predictedAccess{
				location: predictedLocation{
					scope:    *call.InstanceID,
					resource: access.Resource,
					key:      resolveSymbolicKey(call, access.Key),
				},
				write: access.Kind != "read",
			})
		}
	}
	return fp
}

func predictedConflict(left, right staticFootprint) bool {
	for _, a := range left.accesses {
		for _, b := range right.accesses {
			if a.location.scope != b.location.scope || a.location.resource != b.location.resource {
				continue
			}
			keyOverlap := a.location.key == b.location.key || a.location.key == "*" || b.location.key == "*"
			if keyOverlap && (a.write || b.write) {
				return true
			}
		}
	}
	return false
}

type symbolicAccessKey struct {
	block uint64
	tx    int
}

type symbolicAccessIndex map[symbolicAccessKey]staticFootprint

func (x symbolicAccessIndex) footprint(block uint64, tx int) (staticFootprint, bool) {
	fp, ok := x[symbolicAccessKey{block: block, tx: tx}]
	return fp, ok
}

func buildSymbolicAccessIndex(p *SymbolicPredictor, blocks []ExecutionBlock) symbolicAccessIndex {
	out := make(symbolicAccessIndex)
	for _, block := range blocks {
		for i, tx := range block.Transactions {
			out[symbolicAccessKey{block: block.BlockNumber, tx: i}] = p.predictTx(tx)
		}
	}
	return out
}

func symbolicAccessSource(p *SymbolicPredictor) string {
	return fmt.Sprintf("%s (%d documents, %d profiles)", p.sourceDir, p.documentCount, p.profileCount)
}
