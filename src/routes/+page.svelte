<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { Button } from '@/components/ui/button';
  import { listen } from '@tauri-apps/api/event';
  import type PlayerInfo from '$lib/types/PlayerInfo';
  import { checkUpdates } from '$lib/utils';
  import { exit } from '@tauri-apps/plugin-process';
  import Input from '@/components/ui/input/input.svelte';
  import { Label } from '@/components/ui/label';
  import MaterialSymbolsKeyboardArrowDownRounded from '~icons/material-symbols/keyboard-arrow-down-rounded';
  import MaterialSymbolsKeyboardArrowUpRounded from '~icons/material-symbols/keyboard-arrow-up-rounded';
  import toast from 'svelte-hot-french-toast';
  import type AppConfig from '@/types/Config';

  let procState = $state(0);
  let pLocation = $state<PlayerInfo>();
  let ipAddress = $state('');
  let port = $state('');
  let settingsExpanded = $state<boolean>(false);

  $effect(() => {
    checkUpdates();
    invoke<AppConfig>('channel_get_config').then((config) => {
      if (isIpValid(config.ip)) {
        ipAddress = config.ip ?? '';
      }
      if (isPortValid(config.port)) {
        port = `${config.port ?? ''}`;
      }
    });
  });

  function isIpValid(ipAddr?: string): boolean {
    let regexp = /^((25[0-5]|(2[0-4]|1\d|[1-9]|)\d)\.?\b){4}$/g;
    return !(ipAddr !== undefined && !regexp.test(ipAddr));
  }

  function isPortValid(portNumber?: number): boolean {
    return !(
      portNumber !== undefined &&
      (Number.isNaN(portNumber) ||
        !Number.isSafeInteger(portNumber) ||
        0 >= portNumber ||
        portNumber > 65535)
    );
  }

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
  async function applyAndRestart(event: Event) {
    event.preventDefault();
    const handler = async () => {
      const ipAddr = ipAddress === null || ipAddress === '' ? undefined : ipAddress;
      if (!isIpValid(ipAddr)) {
        toast.error('IP 주소 형식이 올바르지 않습니다.');
        return;
      }

      let portNumber = port === null || port === '' ? undefined : +port;
      if (!isPortValid(portNumber)) {
        toast.error('포트 번호가 올바르지 않습니다.');
        return;
      }
      await invoke('write_config', { ip: ipAddr, port: portNumber });
      await invoke('restart_server');
    };
    await handler();
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
  <div class="flex flex-row">
    <Button
      variant="link"
      class="mt-4"
      onclick={() => {
        settingsExpanded = !settingsExpanded;
      }}
    >
      {#if settingsExpanded}
        <MaterialSymbolsKeyboardArrowDownRounded class="" />
      {:else}
        <MaterialSymbolsKeyboardArrowUpRounded class="" />
      {/if}
      고급 설정
    </Button>
  </div>
  {#if settingsExpanded}
    <!-- IP 주소와 포트 수동 설정 UI -->
    <div class="flex flex-row mt-2 space-x-2">
      <div>
        <Label for="ip">IP 주소</Label>
        <Input
          id="ip"
          type="text"
          bind:value={ipAddress}
          placeholder="0.0.0.0"
        />
      </div>
      <div>
        <Label for="port">포트</Label>
        <Input id="port" type="number" bind:value={port} placeholder="46821" />
      </div>
    </div>
    <span>
      <Button class="mt-2" onclick={applyAndRestart}>서버 재시작</Button>
    </span>
  {/if}
</main>
