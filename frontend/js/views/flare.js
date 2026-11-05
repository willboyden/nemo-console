import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
// NVIDIA Flare: NVIDIA's research-prototype inference server on the lab's sm120 GPUs (the RTX PRO 6000s, the
// AITop100's RTX 5090) and sm121 GPUs (the DGX Sparks, one or all four). Every number is per target; the engine
// counters exist only while a target serves.
const DESC = { system: 'flare', charts: ['flare.targets_serving', 'flare.generated_total', 'flare.active_requests', 'flare.draft_acceptance'], tables: ['flare.packages'] };
class NcFlare extends NcView {
  setup() { genericSystemView(this, DESC); }
}
customElements.define('nc-flare', NcFlare);
