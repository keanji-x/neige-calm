#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../../src-tauri/gen/android"
# The preceding Tauri CLI step built this exact native binary and embedded assets.
# Its temporary CLI socket is closed now; instrumentation only rebuilds Kotlin.
test -s app/src/main/jniLibs/x86_64/libapp_lib.so
test -s app/src/main/jniLibs/x86_64/libneige_p2p.so
./gradlew :app:connectedUniversalDebugAndroidTest -x :app:rustBuildX86_64Debug --no-daemon --max-workers=4 \
  -Pneige.launcherSmoke=true -PabiList=x86_64 -ParchList=x86_64 -PtargetList=x86_64 \
  -Pandroid.testInstrumentationRunnerArguments.class=io.neigecalm.next.LauncherConnectionInstrumentationTest
mkdir -p ../../../artifacts
for phase in seed check; do
  adb shell am instrument -w -r \
    -e class io.neigecalm.next.RememberedSessionInstrumentationTest \
    -e phase "$phase" \
    io.neigecalm.next.p2ptrial.test/androidx.test.runner.AndroidJUnitRunner > "../../../artifacts/resume-$phase.txt"
  python3 - "../../../artifacts/resume-$phase.txt" <<'PY'
import pathlib,sys
text=pathlib.Path(sys.argv[1]).read_text()
assert 'OK (1 test)' in text and 'FAILURES' not in text and 'Process crashed' not in text,text
PY
  adb shell am force-stop io.neigecalm.next.p2ptrial
done
