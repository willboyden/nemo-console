import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
// NeMo Gym: reinforcement-learning environments and rollouts, run per command by the lab wrapper. The run counts are
// per-verdict; the reward numbers are those of the newest smoke run that succeeded.
const DESC = { system: 'gym', charts: ['gym.runs', 'gym.live_running', 'gym.mean_reward', 'gym.pass_at_1'], tables: ['gym.runs_by_verdict'] };
class NcGym extends NcView {
  setup() {
    genericSystemView(this, DESC);
  }
}
customElements.define('nc-gym', NcGym);
