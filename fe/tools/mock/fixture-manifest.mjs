export const POSITIVE_FIXTURES = Object.freeze(['additional-properties.json', 'compositions.json', 'path-and-ref.json']);
export const NEGATIVE_FIXTURES = Object.freeze(['broken-ref.json', 'invalid-template.json', 'mismatched-parameters.json', 'no-leading-slash.json', 'no-responses.json', 'unmatched-close.json']);

/** @param {'positive' | 'negative'} kind @param {readonly string[]} manifest @param {readonly string[]} trackedPaths */
export function compareFixtureManifest(kind, manifest, trackedPaths) {
  const trackedNames = trackedPaths.filter((path) => path.startsWith(`tools/mock/fixtures/${kind}/`))
    .map((path) => path.slice(path.lastIndexOf('/') + 1)).sort();
  const expectedNames = [...manifest].sort();
  return trackedNames.length === expectedNames.length && trackedNames.every((name, index) => name === expectedNames[index]) ? ''
    : `${kind} fixture manifest differs from Git\nmanifest: ${expectedNames.join(', ')}\ntracked: ${trackedNames.join(', ')}`;
}
