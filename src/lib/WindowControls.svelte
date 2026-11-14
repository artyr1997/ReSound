<script lang="ts">
	// Desktop only: the window has no title bar and no buttons (src-tauri/tauri.conf.json, lib.rs),
	// only an invisible strip along the top for dragging. ⌘W closes, ⌘M minimises.
	// A window ReDesk pinned to its place (⌃⌥P) has PIN written down its left side.
	import { getCurrentWindow } from '@tauri-apps/api/window';
	import { getCurrentWebview } from '@tauri-apps/api/webview';

	const win = getCurrentWindow();

	// whether this window is the focused one of the active app;
	// the page hears of it too (html.blurred: the window's rim, app.css)
	let focused = $state(true);
	$effect(() => {
		document.documentElement.classList.toggle('blurred', !focused);
	});
	let pinned = $state(false);
	$effect(() => {
		const off = getCurrentWebview().listen<boolean>('pinned', ({ payload }) => (pinned = payload));
		return () => {
			off.then((stop) => stop());
		};
	});
	$effect(() => {
		win.isFocused().then((f) => (focused = f));
		const off = win.onFocusChanged(({ payload }) => (focused = payload));
		return () => {
			off.then((stop) => stop());
		};
	});
</script>

<div class="drag-strip" data-tauri-drag-region></div>

{#if pinned}
	<div class="pin" aria-label="Pinned">PIN</div>
{/if}

<style>
	.drag-strip {
		position: fixed;
		top: 0;
		left: 0;
		right: 0;
		height: 28px;
		z-index: 90;
	}

	/* where the window's buttons were: PIN, down the left side, as the apps' spaced capitals */
	.pin {
		position: fixed;
		top: 14px;
		left: 10px;
		z-index: 95;
		writing-mode: vertical-rl;
		font-family: 'Noto Sans JP', sans-serif;
		font-size: 11px;
		letter-spacing: 0.4em;
		color: var(--text, currentColor);
		opacity: 0.45;
		pointer-events: none;
		user-select: none;
	}
</style>
