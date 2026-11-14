<script lang="ts">
	// Desktop only: the window has no title bar and no buttons (src-tauri/tauri.conf.json, lib.rs),
	// only an invisible strip along the top for dragging. ⌘W closes, ⌘M minimises.
	// The focused window has a diamond in its top left corner; one ReDesk pinned (⌃⌥P) has P under it.
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

<!-- in the corner where macOS has its buttons: a diamond while this window is the focused one (as ReDesk's
     marks for "this one"), and P under it while ReDesk has it pinned (⌃⌥P) -->
<div class="marks" aria-hidden="true">
	<span class="diamond" class:on={focused}></span>
	{#if pinned}<span class="pin">P</span>{/if}
</div>

<style>
	.drag-strip {
		position: fixed;
		top: 0;
		left: 0;
		right: 0;
		height: 28px;
		z-index: 90;
	}

	.marks {
		position: fixed;
		top: 14px;
		left: 14px;
		width: 13px;
		z-index: 95;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 7px;
		pointer-events: none;
		user-select: none;
		transition: opacity 150ms ease;
	}

	/* 13 across, as ReDesk's diamond: a 9pt square on its corner */
	.diamond {
		width: 9px;
		height: 9px;
		margin: 2px 0;
		background: var(--text, currentColor);
		transform: rotate(45deg);
		opacity: 0;
		transition: opacity 150ms ease;
	}

	.diamond.on {
		opacity: 0.55;
	}

	.pin {
		font-family: 'Noto Sans JP', sans-serif;
		font-size: 11px;
		line-height: 1;
		color: var(--text, currentColor);
		opacity: 0.45;
	}
</style>
