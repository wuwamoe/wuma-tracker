<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { Button } from '@/components/ui/button';
  import { listen } from '@tauri-apps/api/event';
  import type PlayerInfo from '$lib/types/PlayerInfo';
  import { checkUpdates } from '$lib/utils';
  import { exit } from '@tauri-apps/plugin-process';

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
  <h1 class="text-2xl mb-2 font-bold">트래커</h1>
  <div class="text-lg">
    상태: {procState == 0 ? '🔴 게임 연결되지 않음' : '🟢 게임 연결됨'}
  </div>
  {#if pLocation}
    <div class="text-base">
      플레이어 위치: {`(${Math.round(pLocation.x / 100)}, ${Math.round(pLocation.y / 100)}, ${Math.round(pLocation.z / 100)})`}
    </div>
  {/if}

  <!-- IP 주소와 포트 수동 설정 UI -->
  <div class="flex flex-col mt-4 space-y-3">
    <div>
      <label for="ip" class="block text-sm font-medium text-gray-700"
        >IP 주소</label
      >
      <input
        id="ip"
        type="text"
        bind:value={ipAddress}
        placeholder="0.0.0.0"
        class="mt-1 block w-full border-gray-300 rounded-md shadow-sm focus:ring-blue-500 focus:border-blue-500"
      />
    </div>
    <div>
      <label for="port" class="block text-sm font-medium text-gray-700"
        >포트</label
      >
      <input
        id="port"
        type="number"
        bind:value={port}
        placeholder="46821"
        class="mt-1 block w-full border-gray-300 rounded-md shadow-sm focus:ring-blue-500 focus:border-blue-500"
      />
    </div>
  </div>
  <div class="flex flex-row space-x-2 mt-4">
    <Button onclick={attach}>연결</Button>
    <Button onclick={quit} variant="destructive">프로그램 종료</Button>
  </div>
</main>
