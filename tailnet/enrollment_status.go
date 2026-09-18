package main

import (
	"encoding/json"
	"io"
)

// No secret-bearing field exists on the stopped-host status channel.
type cleanupReport struct {
	Version        int    `json:"version"`
	PendingCleanup int    `json:"pendingCleanup"`
	Detail         string `json:"detail"`
}

func reportCleanup(writer io.Writer, stateDir string) error {
	dir, err := privateDirectory(stateDir)
	if err != nil {
		return err
	}
	defer dir.Close()
	ledger, err := readLedger(dir)
	if err != nil {
		return err
	}
	return json.NewEncoder(writer).Encode(cleanupReport{Version: 2, PendingCleanup: len(ledger.Records), Detail: cleanupDetail(ledger.Records)})
}
