<script>
  import { hub } from '../lib/home.svelte.js';
  import { setPermission } from '../../lib/hub.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** What a rule names: the device's name, the point's label, the room. */
  function describe(point) {
    const i = point.indexOf('/');
    const device = hub.devices[point.slice(0, i)];
    const key = point.slice(i + 1);
    const spec = device?.points.find((p) => p.key === key);
    return {
      name: device?.label?.name || device?.native_name || point.slice(0, i),
      key: spec?.label ?? key,
      room: device?.label?.room || device?.native_room || '',
    };
  }

  const rules = $derived([...(hub.permissions ?? [])].sort((a, b) => a.point.localeCompare(b.point)));

  // ---- adding a rule: pick a device, one of its writable points, a policy ----
  let deviceId = $state('');
  let pointKey = $state('');
  let policy = $state('always');

  const writable = $derived(
    Object.values(hub.devices)
      .filter((d) => d.points.some((p) => p.access?.write))
      .map((d) => ({ id: d.id, name: d.label?.name || d.native_name || d.id }))
      .sort((a, b) => a.name.localeCompare(b.name, locale()))
  );
  const pointsOf = $derived((hub.devices[deviceId]?.points ?? []).filter((p) => p.access?.write));

  function save() {
    if (!deviceId || !pointKey) return;
    setPermission(`${deviceId}/${pointKey}`, policy);
    deviceId = '';
    pointKey = '';
  }
</script>

<section class="card perms">
  <div class="head">
    <span class="icon"><Icon name="shield" size={20} /></span>
    <div class="what">
      <b>{t('systeme.permissions.titre')}</b>
      <span class="muted">{t('systeme.permissions.intro')}</span>
    </div>
  </div>

  {#if rules.length === 0}
    <p class="muted vide">{t('systeme.permissions.vide')}</p>
  {:else}
    <ul>
      {#each rules as r (r.point)}
        {@const d = describe(r.point)}
        <li>
          <div class="target">
            <b>{d.name}</b>
            <span class="key">{d.key}</span>
            {#if d.room}<span class="muted">{d.room}</span>{/if}
            {#if r.orphaned}<span class="warn">{t('systeme.permissions.orpheline')}</span>{/if}
          </div>
          <div class="choice" role="radiogroup" aria-label={t('systeme.permissions.regle')}>
            <button class:active={r.policy === 'never'} disabled={!hub.session.human} onclick={() => setPermission(r.point, 'never')}>{t('systeme.permissions.jamais')}</button>
            <button disabled={!hub.session.human} onclick={() => setPermission(r.point, 'ask')}>{t('systeme.permissions.sur_demande')}</button>
            <button class:active={r.policy === 'always'} disabled={!hub.session.human} onclick={() => setPermission(r.point, 'always')}>{t('systeme.permissions.toujours')}</button>
          </div>
        </li>
      {/each}
    </ul>
  {/if}

  {#if hub.session.human}
    <div class="add">
      <select bind:value={deviceId} aria-label={t('systeme.permissions.appareil')} onchange={() => (pointKey = '')}>
        <option value="">{t('systeme.permissions.appareil')}</option>
        {#each writable as d (d.id)}<option value={d.id}>{d.name}</option>{/each}
      </select>
      <select bind:value={pointKey} aria-label={t('systeme.permissions.point')} disabled={!deviceId}>
        <option value="">{t('systeme.permissions.point')}</option>
        {#each pointsOf as p (p.key)}<option value={p.key}>{p.label ?? p.key}</option>{/each}
      </select>
      <select bind:value={policy} aria-label={t('systeme.permissions.regle')}>
        <option value="always">{t('systeme.permissions.toujours')}</option>
        <option value="never">{t('systeme.permissions.jamais')}</option>
      </select>
      <button class="primary" disabled={!deviceId || !pointKey} onclick={save}>{t('systeme.permissions.ajouter')}</button>
    </div>
  {:else}
    <p class="hint">{t('systeme.permissions.indice')}</p>
  {/if}
</section>

<style>
  .perms {
    display: grid;
    gap: 14px;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 12px 16px;
  }

  .icon {
    display: grid;
    place-items: center;
    width: 42px;
    height: 42px;
    border-radius: 50%;
    background: var(--cool-soft);
    color: var(--cool);
  }

  .what {
    display: grid;
    flex: 1;
    min-width: 220px;
    gap: 2px;
    font-size: 14px;
  }

  .vide {
    margin: 0;
  }

  ul {
    display: grid;
    gap: 8px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px 16px;
    padding: 8px 0;
    border-top: 1px solid var(--line);
  }

  .target {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 8px;
    font-size: 14px;
  }

  .key {
    color: var(--ink-2);
  }

  .warn {
    font-size: 13px;
    color: #a33;
  }

  .choice {
    display: flex;
    border: 1px solid var(--line);
    border-radius: 999px;
    overflow: hidden;
  }

  .choice button {
    border: none;
    background: transparent;
    padding: 6px 14px;
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }

  .choice button + button {
    border-left: 1px solid var(--line);
  }

  .choice button.active {
    background: var(--ink);
    color: var(--surface);
    font-weight: 700;
  }

  .choice button:disabled {
    cursor: default;
    opacity: 0.7;
  }

  .add {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }

  select {
    font: inherit;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--surface);
    max-width: 220px;
  }

  .primary {
    min-height: 38px;
    padding: 0 18px;
    border: none;
    border-radius: 999px;
    background: var(--ink);
    color: var(--surface);
    font: inherit;
    font-weight: 700;
    cursor: pointer;
  }

  .primary:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .hint {
    margin: 0;
    font-size: 13px;
    color: var(--ink-2);
  }
</style>
