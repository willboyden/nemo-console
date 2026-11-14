import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
import { DCGM } from '../run-descriptors.js';
import { t } from '../i18n.js';
import { fleetTab } from '../dcgm/fleet-tab.js';
import { dcgmHealth } from '../dcgm/health.js';
// DCGM: GPU telemetry from the exporter(s). Per-GPU series (labels gpu / host) become one line each. Since addendum 9.14: a Fleet tab (a
// heatmap of every host Prometheus scrapes) and, on Status, the per-GPU health cards (really busy?, memory, power against its limit, energy,
// throttle violations, remapped rows, the XID and diagnostics timeline) under the page's window picker.
const DESC = {
  system: 'dcgm',
  // The generic cards: one chart per key, a line per GPU (a picker above four). The memory and power charts are replaced by the cards of
  // `extras` (memory stacked, power against its limit); PCIe replays are a rate (the key ends in _total).
  charts: ['dcgm.gpu_util', 'dcgm.gpu_temp_c', 'dcgm.mem_temp_c', 'dcgm.pcie_tx_bps', 'dcgm.pcie_rx_bps', 'dcgm.pcie_replay_total'],
  stats: ['dcgm.gpu_temp_c', 'dcgm.power_w', 'dcgm.gpu_util', 'dcgm.fb_used_b', 'dcgm.xid_last', 'dcgm.xid_total'], // everything else is in the all-values table
  tables: ['dcgm.host_up'], // fleet hosts (labels host, job)
  runHistory: DCGM, // the diagnostic runs (dcgm.diag events)
  extras: (scope, shared) => dcgmHealth(scope, shared).el,
  extraTabs: () => [{ id: 'fleet', label: t('tab.fleet'), render: (scope, ctx) => fleetTab(scope, ctx) }],
};
class NcDcgm extends NcView {
  setup() {
    genericSystemView(this, DESC);
  }
}
customElements.define('nc-dcgm', NcDcgm);
