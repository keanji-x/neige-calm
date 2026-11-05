read go || exit 9
test "$go" = go || exit 9
printf launched >> launches
if test "$SURVIVOR_ONLY" = 1; then
    (
        for i in $(seq 100); do
            test -f finish && break
            sleep .05
        done
    ) &
    printf ready
    for i in $(seq 100); do
        test -f leader-finish && break
        sleep .05
    done
else
    printf ready
    for i in $(seq 100); do
        test -f finish && break
        sleep .05
    done
fi
