package main

import "sort"

// rustCanonicalWriteIndex supports key-driven post-consensus validation. Exact
// reads ask for the latest canonical writer by fingerprint; range reads only
// scan keys in the matching store rather than every earlier transaction.
type rustCanonicalWriteIndex struct {
	exact     map[accessID]int
	locations map[storeID]map[string]int
}

func newRustCanonicalWriteIndex() *rustCanonicalWriteIndex {
	return &rustCanonicalWriteIndex{
		exact:     make(map[accessID]int, 128),
		locations: make(map[storeID]map[string]int, 16),
	}
}

func (x *rustCanonicalWriteIndex) add(transaction int, writes writeSet) {
	if x == nil {
		return
	}
	add := func(id accessID, loc writeLocation) {
		x.exact[id] = transaction
		keys := x.locations[loc.store]
		if keys == nil {
			keys = make(map[string]int)
			x.locations[loc.store] = keys
		}
		keys[string(loc.key)] = transaction
	}
	for id, loc := range writes.exact {
		add(id, loc)
	}
	for id, collisions := range writes.collisions {
		for _, loc := range collisions {
			add(id, loc)
		}
	}
}

func rustWriterInvalid(writer, reader int, visible rustVisibilityMask, replayed []bool) bool {
	return writer >= 0 && writer < reader && (!visible.contains(writer) || replayed[writer])
}

func (x *rustCanonicalWriteIndex) invalidating(reader int, tracker *accessTracker, visible rustVisibilityMask, replayed []bool) []int {
	if x == nil || tracker == nil {
		return nil
	}
	causes := make(map[int]struct{})
	for id := range tracker.reads {
		if writer, ok := x.exact[id]; ok && rustWriterInvalid(writer, reader, visible, replayed) {
			causes[writer] = struct{}{}
		}
	}
	for _, readRange := range tracker.ranges {
		for raw, writer := range x.locations[readRange.store] {
			if !rustWriterInvalid(writer, reader, visible, replayed) {
				continue
			}
			if keyInRange([]byte(raw), readRange.start, readRange.end) {
				causes[writer] = struct{}{}
			}
		}
	}
	out := make([]int, 0, len(causes))
	for writer := range causes {
		out = append(out, writer)
	}
	sort.Ints(out)
	return out
}

func rustInvalidatingByScan(reader int, receipt rustReadyReceipt, finalTrackers []*accessTracker, replayed []bool) []int {
	var invalidating []int
	for predecessor := 0; predecessor < reader; predecessor++ {
		if receipt.visible.contains(predecessor) && !replayed[predecessor] {
			continue
		}
		if finalTrackers[predecessor] != nil && readsConflictWithWrites(receipt.store.tracker, &finalTrackers[predecessor].writes) {
			invalidating = append(invalidating, predecessor)
		}
	}
	return invalidating
}
