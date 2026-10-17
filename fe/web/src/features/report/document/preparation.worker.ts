import type { TrackReport } from '../../../../../core/domain/report.ts';
import { serveWorkerTask } from '../../../systems/worker-task/public.ts';
import { prepareReportDocument, type PreparedReport } from './preparation.ts';

serveWorkerTask<TrackReport | null, PreparedReport>(globalThis, prepareReportDocument);
