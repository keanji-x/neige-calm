import { fileURLToPath } from 'node:url';

const githubPath = './github-squash.ts';
const { githubOwnershipRequest, githubSquashCommits } = await import(githubPath);
const validatorPath = './validator.ts';
const {
  gitOwnershipCommits, ownershipCommitsForEvent, resolveOwnershipBase, validateOwnership, validateOwnershipPullRequestBody,
} = await import(validatorPath);
const { ownershipManifest } = await import('../../ownership-manifest.mjs');

const eventName = process.env.OWNERSHIP_EVENT_NAME;
if (eventName !== 'pull_request' && eventName !== 'push') {
  throw new Error('ownership event audit requires a pull_request or push event');
}
const baseSha = process.env.OWNERSHIP_BASE_SHA ?? '';
const headSha = process.env.OWNERSHIP_HEAD_SHA ?? '';
const repositoryRoot = fileURLToPath(new URL('../../..', import.meta.url));
const base = resolveOwnershipBase(
  repositoryRoot,
  baseSha,
  headSha,
  eventName,
  process.env.OWNERSHIP_PUSH_FORCED === 'true',
);
const commits = await ownershipCommitsForEvent(eventName,
  () => gitOwnershipCommits(repositoryRoot, base, headSha), ownershipManifest,
  /** @param {import('./validator.ts').OwnershipCommit} commit */ (commit) => {
    const repository = process.env.GITHUB_REPOSITORY ?? '';
    return githubSquashCommits(commit, repository, githubOwnershipRequest({
      repository, token: process.env.GITHUB_TOKEN ?? '',
    }));
  });
const encodedBody = process.env.OWNERSHIP_PR_BODY_BASE64;
if (eventName === 'pull_request' && encodedBody === undefined) {
  throw new Error('ownership pull request audit requires the current pull request body');
}
const pullRequestBody = encodedBody === undefined ? '' : Buffer.from(encodedBody, 'base64').toString('utf8');
const violations = [
  ...validateOwnership(ownershipManifest, [], commits),
  ...validateOwnershipPullRequestBody(eventName, commits, pullRequestBody),
];

if (violations.length > 0) {
  throw new Error(`ownership event audit failed:\n${violations
    .map(/** @param {{message: string}} violation */ (violation) => violation.message).join('\n')}\n`
    + 'add exact OWNERSHIP-CHANGE trailers to the commits and preserve them in the pull request body');
}

console.log(`ownership ${eventName} audit: commit trailers valid${eventName === 'pull_request' ? ' and preserved in the pull request body' : ''}`);
