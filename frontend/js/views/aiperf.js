import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
import { AIPERF } from '../run-descriptors.js';
// AIPerf: load-profile results from the newest run (ttft / itl by quantile, throughput); the History tab compares runs.
const DESC = { system: 'aiperf', charts: ['ap.ttft_ms', 'ap.itl_ms', 'ap.latency_ms', 'ap.output_tps', 'ap.request_tps'], runHistory: AIPERF };
class NcAiPerf extends NcView {
  setup() {
    genericSystemView(this, DESC);
  }
}
customElements.define('nc-aiperf', NcAiPerf);
