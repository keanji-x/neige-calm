#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../../src-tauri/gen/android"
./gradlew :app:connectedUniversalDebugAndroidTest --no-daemon --max-workers=4 \
  -PabiList=x86_64 -ParchList=x86_64 -PtargetList=x86_64 \
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
