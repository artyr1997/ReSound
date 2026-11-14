<script lang="ts">
	// A video as ReSurf shows a page: in a card on the paper, its corners round, a light shadow under
	// it; the folder's name above it, and under it the video's name between ← and → (the folder's
	// other videos). The side buttons as ReSurf's: MUSIC at the top left, FULL at the bottom right,
	// which lays the card over nearly the whole window, a thin frame of paper round it, and nothing else.
	// In the card, over the picture's foot, the line and the time, gone while it plays and the mouse
	// rests. The file comes through Tauri's asset protocol, so WebKit plays it itself:
	// MP4 / M4V / MOV (H.264, HEVC), WebM.
	import { onMount } from 'svelte';
	import { convertFileSrc } from '@tauri-apps/api/core';

	let { paths, start = 0, onclose }: { paths: string[]; start?: number; onclose: () => void } = $props();

	// the first one shown (another video opened is a new Video: App.svelte)
	// svelte-ignore state_referenced_locally
	let index = $state(start);
	let element: HTMLVideoElement | undefined = $state();
	let playing = $state(false);
	let time = $state(0);
	let duration = $state(0);
	let failed = $state(false);
	let resting = $state(false);
	let restTimer: ReturnType<typeof setTimeout> | null = null;

	// FULL, kept between videos and launches
	function readFull() {
		try {
			return localStorage.getItem('videoFull') === '1';
		} catch {
			return false;
		}
	}
	let full = $state(readFull());
	function toggleFull() {
		full = !full;
		try {
			localStorage.setItem('videoFull', full ? '1' : '0');
		} catch {}
	}

	const path = $derived(paths[index]);
	const name = $derived((path.split('/').pop() ?? path).replace(/\.[^.]+$/, ''));
	const folder = $derived(path.split('/').slice(-2, -1)[0] ?? '');
	const progress = $derived(duration > 0 ? time / duration : 0);

	// a new file: from its start
	$effect(() => {
		path;
		failed = false;
		time = 0;
		duration = 0;
	});

	function clock(seconds: number) {
		if (!isFinite(seconds) || seconds < 0) seconds = 0;
		const s = Math.floor(seconds % 60).toString().padStart(2, '0');
		const m = Math.floor(seconds / 60) % 60;
		const h = Math.floor(seconds / 3600);
		return h ? `${h}:${m.toString().padStart(2, '0')}:${s}` : `${m}:${s}`;
	}

	function toggle() {
		if (!element) return;
		if (element.paused) element.play().catch(() => {});
		else element.pause();
	}

	function seekBy(seconds: number) {
		if (element) element.currentTime = Math.min(Math.max(element.currentTime + seconds, 0), duration || 0);
	}

	function seekTo(e: MouseEvent) {
		const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
		if (element && duration) element.currentTime = ((e.clientX - rect.left) / rect.width) * duration;
	}

	function step(delta: number) {
		const next = index + delta;
		if (next >= 0 && next < paths.length) index = next;
	}

	// the line in the card rests with the mouse while the video plays
	function wake() {
		resting = false;
		if (restTimer) clearTimeout(restTimer);
		restTimer = setTimeout(() => (resting = playing), 2200);
	}
	$effect(() => {
		if (!playing) resting = false;
		else wake();
	});

	function keydown(e: KeyboardEvent) {
		if (e.metaKey || e.ctrlKey || e.altKey) return;
		wake();
		if (e.key === ' ' || e.key === 'k') {
			e.preventDefault();
			toggle();
		} else if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
			e.preventDefault();
			const back = e.key === 'ArrowLeft';
			if (e.shiftKey) step(back ? -1 : 1);
			else seekBy(back ? -10 : 10);
		} else if (e.code === 'KeyF') {
			e.preventDefault();
			toggleFull();
		} else if (e.key === 'Escape') {
			e.preventDefault();
			// out of FULL first, then back to the music
			if (full) toggleFull();
			else onclose();
		}
	}

	onMount(() => {
		// the window's own keys while the video is up (RePlay's are held, see App.svelte)
		window.addEventListener('keydown', keydown);
		return () => {
			window.removeEventListener('keydown', keydown);
			if (restTimer) clearTimeout(restTimer);
		};
	});
</script>

<div class="video fade-in" class:full>
	<!-- FULL leaves everything but the card: F, a double click or Esc bring the rest back -->
	{#if !full}
		<button class="side plain nav-music" onclick={onclose} title="Back to the music (Esc)">MUSIC</button>
		<button class="side switch nav-full" onclick={toggleFull} title="Video fills the window (F)">FULL</button>
		<!-- a small window: the corners instead -->
		<button class="corner top-right" onclick={onclose} title="Back to the music (Esc)">MUSIC</button>
		<button class="corner bottom-right" onclick={toggleFull} title="Video fills the window (F)">FULL</button>
	{/if}

	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="card" class:resting onmousemove={wake} onmouseleave={() => (resting = playing)}>
		{#key path}
			<!-- svelte-ignore a11y_media_has_caption -->
			<video
				bind:this={element}
				src={convertFileSrc(path)}
				autoplay
				bind:currentTime={time}
				bind:duration
				onplay={() => (playing = true)}
				onpause={() => (playing = false)}
				onended={() => (index < paths.length - 1 ? step(1) : (playing = false))}
				onerror={() => (failed = true)}
				onclick={toggle}
				ondblclick={toggleFull}
			></video>
		{/key}

		{#if failed}
			<p class="failed">This video doesn't play here.</p>
		{:else if !playing}
			<button class="paused" onclick={toggle} title="Play (Space)"><span class="ink-mark">PLAY</span></button>
		{/if}

		<div class="foot">
			<span class="clock">{clock(time)}</span>
			<button class="bar" onclick={seekTo} aria-label="Seek">
				<span class="bar-line"></span>
				<span class="bar-fill" style="width: {progress * 100}%"></span>
			</button>
			<span class="clock">{clock(duration)}</span>
		</div>
	</div>

	{#if !full}
		<p class="title" title={path}>{folder}</p>
		<div class="pager">
			<button class="turn" onclick={() => step(-1)} disabled={index === 0} aria-label="Previous" title="Previous video (⇧←)">←</button>
			<p class="path" title={name}>{name}</p>
			<button class="turn" onclick={() => step(1)} disabled={index === paths.length - 1} aria-label="Next" title="Next video (⇧→)">→</button>
		</div>
	{/if}
</div>

<style>
	.video {
		position: fixed;
		inset: 0;
		z-index: 50;
		background: var(--bg);
	}

	/* the side buttons where ReSurf has NEW and FULL */
	.nav-music { left: calc(6.25% - 28px); top: calc(6.25% + 8px); width: 42px; padding-top: 0.55em; }
	.nav-full { right: calc(6.25% - 28px); bottom: 6.25%; }

	/* ── the card, as ReSurf's page ── */
	.card {
		position: fixed;
		top: calc(6.25% + 24px);
		bottom: calc(6.25% + 24px);
		left: calc(6.25% + 80px);
		right: calc(6.25% + 80px);
		border-radius: 22px;
		overflow: hidden;
		background: #000;
		box-shadow: 0 1px 12px rgba(0, 0, 0, 0.08);
		transition: top 300ms ease, bottom 300ms ease, left 300ms ease, right 300ms ease, border-radius 300ms ease;
	}

	.card.resting {
		cursor: none;
	}

	/* FULL: the card over nearly the whole window, a frame of paper round it (as ReSurf's) */
	.full .card { top: 6px; bottom: 6px; left: 6px; right: 6px; border-radius: 12px; }

	video {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: contain;
		background: #000;
	}

	.failed {
		position: absolute;
		top: 50%;
		width: 100%;
		transform: translateY(-50%);
		text-align: center;
		font-family: 'Kurale', serif;
		font-size: 18px;
		color: #e8e8e8;
		opacity: 0.7;
	}

	/* paused: PLAY in the middle of the picture */
	.paused {
		position: absolute;
		top: 50%;
		left: 50%;
		transform: translate(-50%, -50%);
		background: transparent;
		border: none;
		padding: 0.3em 0.6em;
		font-family: 'Noto Sans JP', sans-serif;
		font-size: 18px;
		letter-spacing: 0.4em;
		cursor: pointer;
	}

	.paused .ink-mark { padding-right: calc(0.45em - 0.4em); }

	/* the line and the time over the picture's foot, on a shade */
	.foot {
		position: absolute;
		left: 0;
		right: 0;
		bottom: 0;
		display: flex;
		align-items: center;
		gap: 14px;
		padding: 48px 24px 16px;
		background: linear-gradient(to top, rgba(0, 0, 0, 0.55), rgba(0, 0, 0, 0));
		color: #e8e8e8;
		transition: opacity 500ms ease;
	}

	.resting .foot {
		opacity: 0;
		pointer-events: none;
	}

	.clock {
		font-size: 14px;
		opacity: 0.85;
		font-variant-numeric: tabular-nums;
		flex-shrink: 0;
	}

	.bar {
		position: relative;
		flex: 1;
		height: 18px;
		background: transparent;
		border: none;
		padding: 0;
		cursor: pointer;
	}

	.bar-line,
	.bar-fill {
		position: absolute;
		left: 0;
		top: 50%;
		height: 2px;
		transform: translateY(-50%);
		background: #e8e8e8;
	}

	.bar-line { right: 0; opacity: 0.3; }

	/* ── the folder above, the video's name under it between ← and →, as ReSurf's ── */
	.title,
	.pager {
		position: fixed;
		left: 50%;
	}

	.title {
		top: calc(3.125% + 12px);
		transform: translate(-50%, -50%);
		max-width: 60%;
		font-family: 'Kurale', serif;
		font-size: 24px;
		line-height: 32px;
		letter-spacing: 0.04em;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.pager {
		bottom: calc(3.125% + 12px);
		transform: translate(-50%, 50%);
		display: flex;
		align-items: center;
		gap: 4rem;
		max-width: 70%;
	}

	.path {
		min-width: 0;
		font-family: 'Kurale', serif;
		font-size: 24px;
		line-height: 32px;
		letter-spacing: 0.04em;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.turn {
		flex-shrink: 0;
		background: transparent;
		border: none;
		font-family: 'Noto Sans JP', sans-serif;
		font-size: 26px;
		line-height: 1;
		opacity: 0.6;
		cursor: pointer;
		transition: opacity 200ms ease;
	}

	.turn:hover:not(:disabled) { opacity: 1; }
	.turn:disabled { opacity: 0.18; cursor: default; }

	.corner {
		position: fixed;
		display: none;
		background: transparent;
		border: none;
		padding: 0.4em 0.3em 0.1em;
		font-family: 'Noto Sans JP', sans-serif;
		font-size: 12px;
		letter-spacing: 0.3em;
		writing-mode: vertical-rl;
		text-orientation: upright;
		line-height: 1;
		opacity: 0.6;
		cursor: pointer;
	}

	.corner:hover { opacity: 1; }
	.corner.top-right { top: 12px; right: 12px; }
	.corner.bottom-right { bottom: 12px; right: 12px; }

	/* a small window, as ReSurf's: no side buttons, a smaller card and words */
	@media (max-width: 759px), (max-height: 559px) {
		.side { display: none; }
		.corner { display: block; }

		.video:not(.full) .card {
			top: 44px;
			bottom: 44px;
			left: 36px;
			right: 36px;
			border-radius: 14px;
		}

		.title {
			top: 22px;
			max-width: calc(100% - 200px);
			font-size: 16px;
			line-height: 24px;
		}

		.pager {
			bottom: 22px;
			gap: 1.5rem;
			max-width: calc(100% - 200px);
		}

		.path {
			font-size: 16px;
			line-height: 24px;
		}

		.turn { font-size: 20px; }
	}
</style>
