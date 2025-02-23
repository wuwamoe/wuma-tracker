<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { Button } from '@/components/ui/button';
  import { listen } from '@tauri-apps/api/event';
  import type PlayerInfo from '$lib/types/PlayerInfo';
  import { checkUpdates } from '$lib/utils';
  import { exit } from '@tauri-apps/plugin-process';
  import Input from '@/components/ui/input/input.svelte';
  import { Label } from '@/components/ui/label';

  let procState = $state(0);
  let pLocation = $state<PlayerInfo>();
  let ipAddress = $state('');
  let port = $state('');

  $effect(() => {
    checkUpdates();
  });

  async function attach(event: Event) {
    event.preventDefault();
    invoke('find_and_attach')
      .then(() => {
        procState = 1;
      })
      .catch(() => {
        procState = 0;
      });
  }
  async function quit() {
    await exit(0);
  }

  listen<PlayerInfo>('handle-location-change', (e) => {
    pLocation = e.payload;
  });
</script>

<main class="flex flex-col py-2 px-4">
  <h1 class="text-2xl mb-2 font-bold">명조 맵스 트래커</h1>
  <div class="text-lg">
    상태: {procState == 0 ? '🔴 게임 연결되지 않음' : '🟢 게임 연결됨'}
  </div>
  {#if pLocation}
    <div class="text-base">
      플레이어 위치: {`(${Math.round(pLocation.x / 100)}, ${Math.round(pLocation.y / 100)}, ${Math.round(pLocation.z / 100)})`}
    </div>
  {/if}

  <div class="flex flex-row space-x-2 mt-2">
    <Button onclick={attach}>연결</Button>
    <Button onclick={quit} variant="destructive">프로그램 종료</Button>
  </div>
<div class="mt-4 text-md">고급 설정</div>
    <!-- IP 주소와 포트 수동 설정 UI -->
    <div class="flex flex-row mt-2 space-x-2">
      <div>
        <Label for="ip">IP 주소</Label>
        <Input id="ip" type="text" bind:value={ipAddress} placeholder="0.0.0.0" />
      </div>
      <div>
        <Label for="port">포트</Label>
        <Input id="port" type="number" bind:value={port} placeholder="46821" />
      </div>
    </div>
</main>
