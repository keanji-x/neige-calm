package main

import "context"

func verifyDirectAbsence(ctx context.Context, family, host string) error {
	return verifyAndroidDirectAbsence(ctx, family, host)
}
