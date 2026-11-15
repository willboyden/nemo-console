import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
import { AIPERF } from '../run-descriptors.js';
import { aiperfDetail } from '../runviz/aiperf-detail.js';
// AIPerf: load-profile results from the newest run (ttft / itl by quantile, throughput); the History tab compares runs and, since addendum 9.15, shows the selected
// run's percentile curves, HTTP phases and per-request view, and the throughput-vs-latency scatter over the runs.
const DESC = { system: 'aiperf', charts: ['ap.ttft_ms', 'ap.itl_ms', 'ap.latency_ms', 'ap.output_tps', 'ap.request_tps'], runHistory: { ...AIPERF, detail: aiperfDetail } };
class NcAiPerf extends NcView {
  setup() {
    genericSystemView(this, DESC);
  }
}
customElements.define('nc-aiperf', NcAiPerf);
