#!/usr/bin/env python3
"""Run only the installed .instrumented app; preserve data across actual process death.

Install the matching app/test APKs first. This driver deliberately cannot install,
clear app data, change network settings, or address the release/.p2ptrial packages.
Each runner invocation must report a complete OK before external force-stop.
"""
import argparse
import json
import re
import subprocess
from pathlib import Path

APP = "io.neigecalm.next.instrumented"
RUNNER = APP + ".test/androidx.test.runner.AndroidJUnitRunner"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--adb", required=True, help="Explicit adb executable")
    parser.add_argument("--serial", required=True, help="Explicit authorized test device")
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    stages = []
    process_boundary = {}

    def adb(*command, timeout=90):
        return subprocess.run([args.adb, "-s", args.serial, *command],
                              text=True, capture_output=True, timeout=timeout, check=False)

    def stop():
        result = adb("shell", "am", "force-stop", APP, timeout=30)
        if result.returncode != 0:
            raise RuntimeError("Could not stop the isolated instrumented package")

    def stage(label, test):
        result = adb("shell", "am", "instrument", "-w", "-r", "-e", "clearPackageData", "false",
                     "-e", "class", test, RUNNER)
        output = result.stdout + result.stderr
        (args.output_dir / (label + ".txt")).write_text(output)
        ok = (result.returncode == 0 and re.search(r"OK \(\s*1 tests?\s*\)", output)
              and re.search(r"INSTRUMENTATION_CODE:\s*-1", output)
              and "Process crashed" not in output and "FAILURES!!!" not in output)
        pid = re.search(r"INSTRUMENTATION_STATUS: recoveryPid=(\d+)", output)
        stages.append({"stage": label, "test": test, "completeRunnerOK": bool(ok),
                       "pid": int(pid.group(1)) if pid else None})
        print(label + (": OK" if ok else ": FAILED"), flush=True)
        if not ok:
            raise RuntimeError("Incomplete or failed runner: " + label)

    try:
        if adb("shell", "pm", "path", APP, timeout=30).returncode != 0:
            raise RuntimeError("Install the isolated instrumented APKs first")
        stop()
        stage("cold-entry", "io.neigecalm.next.RecoveryInstrumentationTest#offlineColdEntryShowsSavedTrackAndCornerStatusBeforeNetworkIsReady")
        stop()
        stage("hot-and-recreate", "io.neigecalm.next.RecoveryInstrumentationTest#historySurvivesColdRecreationAndHotResumeKeepsTheSameWebView")
        stop()
        stage("seed", "io.neigecalm.next.RecoveryProcessInstrumentationTest#seedSavedRoute")
        seed_pid = stages[-1]["pid"]
        if seed_pid is None:
            raise RuntimeError("The seed runner did not report its actual PID")
        # Android ends the instrumented target after a completed runner. Start
        # the ordinary Activity so force-stop demonstrably kills a live app.
        launched = adb("shell", "am", "start", "-W", "-n", APP + "/io.neigecalm.next.MainActivity", timeout=30)
        (args.output_dir / "normal-app-start.txt").write_text(launched.stdout + launched.stderr)
        live_pids = adb("shell", "pidof", APP, timeout=30).stdout.split()
        if launched.returncode != 0 or len(live_pids) != 1 or not live_pids[0].isdigit():
            raise RuntimeError("The ordinary app did not have one live PID before force-stop")
        normal_pid = int(live_pids[0])
        process_boundary.update({"seedPid": seed_pid, "normalAppPid": normal_pid,
                                 "normalAppAliveBeforeForceStop": True})
        stop()  # No pm clear: only the live process ends; the real pointer survives.
        if adb("shell", "pidof", APP, timeout=30).stdout.strip():
            raise RuntimeError("The seed process survived force-stop")
        process_boundary["goneAfterForceStop"] = True
        stage("check-new-process", "io.neigecalm.next.RecoveryProcessInstrumentationTest#checkSavedRouteAfterProcessDeath")
        check_pid = stages[-1]["pid"]
        process_boundary["checkPid"] = check_pid
        if check_pid is None or check_pid in (seed_pid, normal_pid):
            raise RuntimeError("Missing evidence of a new process after the live app was force-stopped")
    finally:
        (args.output_dir / "result.json").write_text(json.dumps({"package": APP, "stages": stages, "processBoundary": process_boundary}, indent=2) + "\n")
        stop()
    print(str(args.output_dir / "result.json"), flush=True)


if __name__ == "__main__":
    main()
