<script lang="ts">
  import type { Caption } from '$proto/Caption';
  import type { Column } from '$proto/Column';
  import { carry, reveal, upTo } from '../lib/reveal';
  import { langName, t } from '../lib/words.svelte';

  // How many finished lines a column keeps, by layout. In a conversation the first column
  // turns to face the person across the table.
  const KEEP = { Reading: 6, Audience: 1, Conversation: 2 };
  let { columns, layout }: { columns: Column[]; layout: keyof typeof KEEP } = $props();
  const still = globalThis.matchMedia?.('(prefers-reduced-motion: reduce)').matches;

  // Each column's tail as it came, how far its reveal stood then, and when: the line still being
  // said is revealed unit by unit at its language's rate, and what was on screen stays.
  type Mark = { tail: Caption; from: number; at: number; rate: number };
  const pos = (m: Mark, at: number) => reveal(m.tail.units.length, m.from, at - m.at, m.rate);
  let last = new Map<string, Mark>();
  const marks = $derived.by(() => {
    const at = performance.now(), next = new Map<string, Mark>();
    for (const c of columns.filter((c) => c.tail)) {
      const was = last.get(c.language), p = was ? pos(was, at) : 0;
      const from = was ? carry(was.tail.text.slice(0, upTo(was.tail.units, p)), p, c.tail!) : 0;
      next.set(c.language, { tail: c.tail!, from, at, rate: c.rate });
    }
    return (last = next);
  });
  let now = $state(performance.now());
  $effect(() => {
    if (still) return;
    let id = requestAnimationFrame(function tick(at) {
      now = at;
      id = requestAnimationFrame(tick);
    });
    return () => cancelAnimationFrame(id);
  });
  const tail = (c: Column, m = marks.get(c.language)!) => (still ? m.tail.text : m.tail.text.slice(0, upTo(m.tail.units, pos(m, now))));
</script>

<div class="captions {layout}">
  {#each columns as c (c.language)}
    <section lang={c.language}>
      <h3>{langName(c.language, c.language)}</h3>
      {#each c.lines.slice(-KEEP[layout]) as line, i (i)}<p class="old">{line.text}</p>{/each}
      {#if c.tail}<p>{tail(c)}</p>{/if}
    </section>
  {:else}
    <p class="quiet">{t('listening')}</p>
  {/each}
</div>
