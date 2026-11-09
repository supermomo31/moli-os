<script>
  import { onMount } from 'svelte';
  import { home, note } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** The stream quality the music service resolves to (Qobuz's
   *  `format_id`): chosen here by a person (code asked), kept in Moli's
   *  encrypted store, applied to the next `play` at once. */
  let state = $state(null);
  let chosen = $state(null);
  let busy = $state(false);

  const LABELS = { 5: 'q5', 6: 'q6', 7: 'q7', 27: 'q27' };

  async function load() {
    try {
      const res = await fetch('/api/integrations/qobuz');
      if (res.ok) {
        state = await res.json();
        chosen = state.format;
      }
    } catch {
      /* the card stays hidden */
    }
  }
  onMount(load);

  async function save() {
    busy = true;
    try {
      const res = await fetch('/api/integrations/qobuz', {
        method: 'PUT',
        headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' },
        body: JSON.stringify({ format_id: Number(chosen) }),
      });
      if (res.status === 403) {
        // A person's gesture: the code first, then the same request again.
        home.held = { label: t('systeme.qobuz.cle_titre'), reason: 'ton code', custom: save };
        return;
      }
      const body = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(body.error ?? res.statusText);
      note(t('systeme.qobuz.enregistre'), 'info');
      await load();
    } catch (e) {
      note(t('systeme.qobuz.erreur', { message: e.message }), 'error');
      chosen = state?.format;
    } finally {
      busy = false;
    }
  }
</script>

{#if state?.formats?.length}
  <section class="quality">
    <div class="row">
      <span class="ico"><Icon name="music" size={18} /></span>
      <div class="text">
        <b>{t('systeme.qobuz.titre')}</b>
        <span class="muted">{t('systeme.qobuz.description')}</span>
      </div>
      <select bind:value={chosen} disabled={busy} aria-label={t('systeme.qobuz.choisir')} onchange={save}>
        {#each state.formats as f (f)}
          <option value={f}>{t(`systeme.qobuz.${LABELS[f] ?? 'q' + f}`)}</option>
        {/each}
      </select>
    </div>
  </section>
{/if}

<style>
  .quality {
    display: grid;
    gap: 14px;
    padding: 16px 18px;
    border-radius: var(--r-md);
    background: var(--warm-soft);
  }

  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  .ico {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface);
    color: var(--warm-ink);
  }

  .text {
    display: grid;
    gap: 2px;
    flex: 1;
    min-width: 220px;
  }

  .text .muted {
    font-size: 13px;
  }

  select {
    font: inherit;
    font-size: 14px;
    font-weight: 650;
    padding: 9px 12px;
    border-radius: var(--r-sm);
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink);
  }
</style>
