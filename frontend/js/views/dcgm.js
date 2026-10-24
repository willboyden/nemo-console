import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
// DCGM: GPU telemetry from the exporter(s). Per-GPU series (labels gpu / host) become one line each.
const DESC = {
  system: 'dcgm',
  charts: ['dcgm.gpu_util', 'dcgm.gpu_temp_c', 'dcgm.power_w', 'dcgm.fb_used_b'],
  stats: ['dcgm.gpu_temp_c', 'dcgm.power_w', 'dcgm.gpu_util', 'dcgm.fb_used_b', 'dcgm.xid_last', 'dcgm.xid_total'],   // everything else is in the all-values table
  tables: ['dcgm.host_up'],   // fleet hosts (labels host, job)
};
class NcDcgm extends NcView {
  setup() { genericSystemView(this, DESC); }
}
customElements.define('nc-dcgm', NcDcgm);
