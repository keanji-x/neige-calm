//go:build !android

package main

import "context"

func verifyDirectAbsence(context.Context, string, string) error { return nil }
