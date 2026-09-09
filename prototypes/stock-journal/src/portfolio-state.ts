import snapshot from './portfolio-snapshot.json';
import { portfolioSnapshotSchema } from './portfolio-framework';

// Single data input shared by the native Report and the sandboxed chart bundle.
export const portfolioSnapshot = portfolioSnapshotSchema.parse(snapshot);
