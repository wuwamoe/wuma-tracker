<script lang="ts">
    import { invoke } from "@tauri-apps/api/core";
    import { Button } from "@/components/ui/button";
    import { listen } from "@tauri-apps/api/event";
    import type PlayerInfo from "src/lib/types/PlayerInfo";

    let procState = $state(0);
    let pLocation = $state<PlayerInfo>();

    async function attach(event: Event) {
        event.preventDefault();
        invoke("find_and_attach")
            .then(() => {
                procState = 1;
            })
            .catch(() => {
                procState = 0;
            });
    }
    async function getLocation(event: Event) {
        event.preventDefault();
        const res = await invoke<PlayerInfo>("get_location");
        console.log(res);
        pLocation = res;
    }

    listen<PlayerInfo>("handle-location-change", (e) => {
        console.log(`data tx:${e.payload}`);
        pLocation = e.payload;
    });
</script>

<main class="flex flex-col py-2 px-4">
    <h1 class="text-2xl mb-2 font-bold">명조 맵스 서포터</h1>
    <div class="text-lg">
        상태: {procState == 0 ? "🔴 게임 연결되지 않음" : "🟢 게임 연결됨"}
    </div>

    {#if pLocation}
        <div class="text-base">
            플레이어 위치: {`(${Math.round(pLocation.x / 100)},${Math.round(pLocation.y / 100)},${Math.round(pLocation.z / 100)})`}
        </div>
    {/if}

    <div class="flex flex-row space-x-2 mt-4">
        <Button onclick={attach}>연결</Button>
    </div>
</main>
