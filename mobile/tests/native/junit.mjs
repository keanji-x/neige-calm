import { execFileSync } from 'node:child_process';

export function readJUnit(directory) {
  return JSON.parse(execFileSync('python3', ['-c',
    'import pathlib,sys,json,xml.etree.ElementTree as E\nrows={}\nfor p in pathlib.Path(sys.argv[1]).rglob("TEST-*.xml"):\n for t in E.parse(p).getroot().iter("testcase"):\n  key=t.attrib["classname"]+"#"+t.attrib["name"]\n  rows[key]="failed" if t.find("failure") is not None or t.find("error") is not None else "skipped" if t.find("skipped") is not None else "passed"\nprint(json.dumps(rows))', directory], { encoding: 'utf8' }));
}
