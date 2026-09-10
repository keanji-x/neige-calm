#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../../src-tauri/gen/android"
# The preceding Tauri CLI step built this exact native binary and embedded assets.
# Its temporary CLI socket is closed now; instrumentation only rebuilds Kotlin.
test -s app/src/main/jniLibs/x86_64/libapp_lib.so
test -s app/src/main/jniLibs/x86_64/libneige_p2p.so
./gradlew :app:testUniversalDebugUnitTest --tests io.neigecalm.next.ConnectionAttemptTest -x :app:rustBuildX86_64Debug --no-daemon --max-workers=4 -Pneige.launcherSmoke=true -PabiList=x86_64 -ParchList=x86_64 -PtargetList=x86_64
./gradlew :app:connectedUniversalDebugAndroidTest -x :app:rustBuildX86_64Debug --no-daemon --max-workers=4 \
  -Pneige.launcherSmoke=true -PabiList=x86_64 -ParchList=x86_64 -PtargetList=x86_64 \
  -Pandroid.testInstrumentationRunnerArguments.class=io.neigecalm.next.LauncherConnectionInstrumentationTest,io.neigecalm.next.DirectConnectionInstrumentationTest
mkdir -p ../../../artifacts
# Gradle may uninstall its test APK after the connected run. Install the exact
# built pair again and discover the actual instrumentation package from Android.
adb install -r -t app/build/outputs/apk/universal/debug/app-universal-debug.apk
mapfile -t test_apks < <(find app/build/outputs/apk/androidTest -name '*.apk' -type f)
test "${#test_apks[@]}" -eq 1
adb install -r -t "${test_apks[0]}"
adb shell pm list instrumentation > ../../../artifacts/resume-instrumentation.txt
instrumentation=$(python3 - ../../../artifacts/resume-instrumentation.txt <<'PY'
import pathlib,re,sys
text=pathlib.Path(sys.argv[1]).read_text()
matches=re.findall(r'instrumentation:([A-Za-z0-9_./]+) \(target=io\.neigecalm\.next\.p2ptrial\)',text)
assert len(matches)==1,text
assert matches[0].endswith('/androidx.test.runner.AndroidJUnitRunner')
print(matches[0])
PY
)
for phase in seed check; do
  adb shell am instrument -w -r \
    -e class io.neigecalm.next.RememberedSessionInstrumentationTest \
    -e phase "$phase" \
    "$instrumentation" > "../../../artifacts/resume-$phase.txt"
  python3 - "../../../artifacts/resume-$phase.txt" <<'PY'
import pathlib,sys
text=pathlib.Path(sys.argv[1]).read_text()
assert 'OK (1 test)' in text and 'FAILURES' not in text and 'Process crashed' not in text,text
PY
  adb shell am force-stop io.neigecalm.next.p2ptrial
done
