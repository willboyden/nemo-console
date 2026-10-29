import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
// Model Optimizer: offline compression jobs (quantization, REAP expert pruning, Minitron). The job counts are
// per-verdict, the size numbers are one per finished job.
const DESC = { system: 'modelopt', charts: ['mo.jobs', 'mo.live_running', 'mo.output_bytes', 'mo.compression_ratio'], tables: ['mo.jobs_by_verdict'] };
class NcModelOpt extends NcView {
  setup() { genericSystemView(this, DESC); }
}
customElements.define('nc-modelopt', NcModelOpt);
