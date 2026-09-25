const { invoke } = window.__TAURI__.core;
const BUILD = 'v1';
const $ = (selector) => document.querySelector(selector);

let tally = await invoke('get_tally');

// rusty-mirror:begin
// Debug builds only: hand our state to a mirror, and accept it back when we are one.
window.__RUSTY_MIRROR_SNAPSHOT__ = () => ({ tally });
if (window.__RUSTY_MIRROR_SEED__?.state) tally = window.__RUSTY_MIRROR_SEED__.state.tally;
// rusty-mirror:end

function render() {
  $('#count').textContent = String(tally);
  $('#build').textContent = BUILD;
}

$('#plus').addEventListener('click', () => {
  tally += 1;
  render();
});

$('#save').addEventListener('click', async () => {
  try {
    await invoke('save_tally', { value: tally });
    $('#status').textContent = `Saved ${tally}.`;
  } catch (error) {
    $('#status').textContent = String(error);
  }
});

render();
