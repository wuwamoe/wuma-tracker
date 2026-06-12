<script lang="ts">
	import type { HTMLAttributes } from "svelte/elements";
	import { cn } from "$lib/utils.js";

	let { class: className, children, ...restProps }: HTMLAttributes<HTMLDivElement> = $props();
</script>

<div
	class={cn("flex flex-row justify-end gap-2", className)}
	{...restProps}
>
	{@render children?.()}
</div>
