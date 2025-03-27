<script lang="ts">
	import { ModeWatcher } from 'mode-watcher';
	import { Toaster } from 'svelte-5-french-toast';
	import '../app.css';
	let { children } = $props();
</script>

<ModeWatcher />
<Toaster />
{@render children()}
