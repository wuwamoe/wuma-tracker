<script lang="ts">
  // Tauri API 및 플러그인 임포트
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { exit } from '@tauri-apps/plugin-process';
  import { getVersion } from '@tauri-apps/api/app';

  // Svelte 및 라이브러리 임포트
  import toast from 'svelte-5-french-toast';

  // 컴포넌트 임포트 (shadcn-svelte 등)
  import { Button } from '@/components/ui/button';
  import Input from '@/components/ui/input/input.svelte';
  import { Label } from '@/components/ui/label';
  // Checkbox 임포트 추가
  import { Badge } from '@/components/ui/badge';
  import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';

  // 아이콘 임포트
  import MaterialSymbolsKeyboardArrowDownRounded from '~icons/material-symbols/keyboard-arrow-down-rounded';
  import MaterialSymbolsKeyboardArrowUpRounded from '~icons/material-symbols/keyboard-arrow-up-rounded';
  import IconConnection from '~icons/mdi/connection';
  import IconPower from '~icons/mdi/power';
  import IconRestart from '~icons/mdi/restart';
  // import IconServer from '~icons/mdi/server'; // 서버 상태 아이콘
  // 연결 URL 아이콘 추가

  // 타입 및 유틸리티 임포트
  import type PlayerInfo from '$lib/types/PlayerInfo';
  import type AppConfig from '@/types/Config';
  import { checkUpdates } from '$lib/utils';
  import type GlobalState from '@/types/GlobalState';
  import { Checkbox } from '@/components/ui/checkbox';

  let globalState = $state<GlobalState>({
    procState: 0,
    serverState: 0,
  });
  let pLocation = $state<PlayerInfo>(); // 플레이어 위치 정보
  let ipAddress = $state(''); // IP 주소 입력값
  let port = $state(''); // 포트 번호 입력값
  // let useSecureConnection = $state(false); // 보안 연결(HTTPS/WSS) 사용 여부
  let settingsExpanded = $state<boolean>(false); // 고급 설정 확장 여부
  let trackerError = $state(''); // 트래커 오류 메시지
  let appversion = $state(''); // 앱 버전
  let autoAttachEnabled = $state(false);

  async function silentAttach() {
    try {
      await invoke('find_and_attach');
      trackerError = '';
      toast.success('자동으로 게임에 연결되었습니다.');
    } catch (err) {
      console.error('Auto-attach failed:', err);
    }
  }

  // 게임 연결 폴링
  $effect(() => {
    let intervalId: ReturnType<typeof setInterval> | undefined;

    // 자동 연결 기능이 활성화되어 있고, 게임이 연결되지 않았을 때만 인터벌 시작
    if (autoAttachEnabled && globalState.procState === 0) {
      intervalId = setInterval(() => {
        if (globalState.procState === 0) {
          console.log('자동으로 게임 연결을 시도합니다...');
          silentAttach();
        }
      }, 5000); // 5초
    }

    // 이 effect가 다시 실행되기 전이나 컴포넌트가 사라질 때 기존 인터벌 정리
    return () => {
      if (intervalId) {
        clearInterval(intervalId);
      }
    };
  });

  // --- 초기화 및 설정 로드 ---
  $effect(() => {
    // 업데이트 확인
    checkUpdates();

    // 저장된 설정 불러오기
    invoke<AppConfig>('channel_get_config')
      .then((config) => {
        if (isIpValid(config.ip)) {
          ipAddress = config.ip ?? '';
        }
        if (isPortValid(config.port)) {
          port = `${config.port ?? ''}`;
        }
        // 보안 연결 설정 불러오기 (기본값 false)
        // useSecureConnection = config.useSecureConnection ?? false;
        autoAttachEnabled = config.autoAttachEnabled ?? false;
      })
      .catch((err) => {
        console.error('Failed to load config:', err);
        toast.error('설정 불러오기 실패');
      });

    // 전역 상태 동기화
    invoke<GlobalState>('channel_get_global_state')
      .then((state) => {
        globalState = state;
      })
      .catch((err) => {
        console.error('전역 상태 동기화 실패:', err);
      });

    // 앱 버전 가져오기
    getVersion()
      .then((x) => (appversion = x))
      .catch((err) => {
        console.error('Failed to get app version:', err);
      });

    // --- 이벤트 리스너 설정 ---
    // 플레이어 위치 변경 리스너
    const unlistenLocation = listen<PlayerInfo>(
      'handle-location-change',
      (e) => {
        pLocation = e.payload;
      },
    );
    // 트래커 오류 리스너
    const unlistenError = listen<string>('handle-tracker-error', (e) => {
      trackerError = e.payload;
    });
    const unlistenServerState = listen<GlobalState>(
      'handle-global-state-change',
      (e) => {
        globalState = e.payload;
      },
    );

    // 컴포넌트 언마운트 시 리스너 해제
    return () => {
      unlistenLocation.then((f) => f());
      unlistenError.then((f) => f());
      unlistenServerState.then((f) => f());
    };
  });

  // --- 유효성 검사 함수 ---
  function isIpValid(ipAddr?: string): boolean {
    const regexp = /^((25[0-5]|(2[0-4]|1\d|[1-9]|)\d)\.?\b){4}$/;
    return ipAddr === undefined || ipAddr === '' || regexp.test(ipAddr);
  }

  function isPortValid(portNumber?: number): boolean {
    return (
      portNumber === undefined ||
      (!Number.isNaN(portNumber) &&
        Number.isSafeInteger(portNumber) &&
        portNumber > 0 &&
        portNumber <= 65535)
    );
  }

  // --- 이벤트 핸들러 ---
  async function attach(event: Event) {
    event.preventDefault();
    toast.promise(invoke('find_and_attach'), {
      loading: '게임 프로세스 찾는 중...',
      success: () => {
        trackerError = '';
        return '게임에 성공적으로 연결되었습니다.';
      },
      error: (err) => {
        console.error('Attach failed:', err);
        return `게임 연결 실패: ${err instanceof Error ? err.message : String(err)}`;
      },
    });
  }

  async function applyAndRestart(event: Event) {
    event.preventDefault();

    const ipAddr = ipAddress.trim() === '' ? undefined : ipAddress.trim();
    if (!isIpValid(ipAddr)) {
      toast.error('IP 주소 형식이 올바르지 않습니다. (예: 127.0.0.1)');
      return;
    }

    const portNumber = port.trim() === '' ? undefined : +port.trim();
    if (port.trim() !== '' && !isPortValid(portNumber)) {
      toast.error('포트 번호가 올바르지 않습니다. (1 ~ 65535 사이의 숫자)');
      return;
    }

    // 설정 저장 및 서버 재시작 로직
    const handler = async () => {
      // write_config 호출 시 useSecureConnection 값 포함
      await invoke('write_config', {
        ip: ipAddr,
        port: portNumber,
        useSecureConnection: null, // 보안 연결 설정 저장
        autoAttachEnabled: autoAttachEnabled,
      });
      await invoke('restart_server'); // 이 호출 후 백엔드가 상태 변경 이벤트를 보내야 함
    };

    toast.promise(handler(), {
      loading: '설정 저장 및 서버 재시작 중...',
      success: '설정이 적용되었고 서버가 재시작되었습니다.',
      error: (err) => {
        console.error('Apply and restart failed:', err);
        return `작업 실패: ${err instanceof Error ? err.message : String(err)}`;
      },
    });
  }

  async function quit() {
    await exit(0);
  }

  // --- 추가: URL 복사 함수 ---
  async function copyUrlToClipboard() {
    if (!globalState.connectionUrl) return;
    try {
      await navigator.clipboard.writeText(globalState.connectionUrl);
      toast.success('연결 URL이 클립보드에 복사되었습니다.');
    } catch (err) {
      console.error('Failed to copy URL: ', err);
      toast.error('URL 복사에 실패했습니다.');
    }
  }
</script>

<main class="container max-w-xl mx-auto py-6 px-4 space-y-6">
  <div class="space-y-4 p-4 border rounded-lg bg-background shadow-sm">
    <div class="flex items-center justify-between pb-2 border-b">
      <h1 class="text-xl font-semibold">명조 맵스 트래커</h1>
      {#if appversion}
        <Badge variant="outline">v{appversion}</Badge>
      {/if}
    </div>

<div class="flex items-center space-x-2">
    <div
        class={`w-3 h-3 rounded-full flex-shrink-0 ${globalState.procState === 0 ? 'bg-red-500' : 'bg-green-500'}`}
    ></div>
    <p class="font-medium text-md">
        {globalState.procState === 0 ? '게임 연결되지 않음' : '게임 연결됨'}
    </p>
    {#if autoAttachEnabled && globalState.procState === 0}
        <Badge variant="secondary" class="flex items-center gap-1">
            <IconRestart class="h-3 w-3 animate-spin" />
            자동 연결 중
        </Badge>
    {/if}
</div>

    <div class="flex items-center space-x-2">
      <div
        class={`w-3 h-3 rounded-full flex-shrink-0 ${globalState.serverState === 0 ? 'bg-red-500' : 'bg-green-500'}`}
      ></div>
      <p class="font-medium text-md">
        {globalState.serverState === 0 ? '통신 서버 비활성' : '통신 서버 활성'}
      </p>
    </div>

    {#if globalState.procState === 1 && pLocation}
      <div class="bg-muted p-3 rounded-md text-sm mt-2">
        <p class="font-medium mb-2">플레이어 위치</p>
        <div class="grid grid-cols-3 gap-2 text-center">
          <div class="bg-background p-1.5 rounded">
            X: {Math.round(pLocation.x / 100)}
          </div>
          <div class="bg-background p-1.5 rounded">
            Y: {Math.round(pLocation.y / 100)}
          </div>
          <div class="bg-background p-1.5 rounded">
            Z: {Math.round(pLocation.z / 100)}
          </div>
        </div>
      </div>
    {/if}

    <div class="flex space-x-3 pt-2">
      <Button class="flex-1" onclick={attach}>
        <IconConnection class="mr-2 h-4 w-4" />
        {#if globalState.procState === 1}
          게임 재연결
        {:else}
          게임 연결
        {/if}
      </Button>
      <Button variant="destructive" class="flex-1" onclick={quit}>
        <IconPower class="mr-2 h-4 w-4" />
        프로그램 종료
      </Button>
    </div>
  </div>

  <div class="space-y-4">
    <Button
      variant="ghost"
      class="w-full justify-between text-left px-3 py-2 border rounded-lg"
      onclick={() => (settingsExpanded = !settingsExpanded)}
      aria-expanded={settingsExpanded}
      aria-controls="advanced-settings"
    >
      고급 설정
      {#if settingsExpanded}
        <MaterialSymbolsKeyboardArrowUpRounded class="h-5 w-5" />
      {:else}
        <MaterialSymbolsKeyboardArrowDownRounded class="h-5 w-5" />
      {/if}
    </Button>

    {#if settingsExpanded}
      <div
        id="advanced-settings"
        class="space-y-4 p-4 border rounded-lg bg-background shadow-sm"
      >
        {#if trackerError}
          <Alert variant="destructive">
            <AlertTitle>트래커 오류</AlertTitle>
            <AlertDescription>{trackerError}</AlertDescription>
          </Alert>
        {:else}
          <div class="text-sm text-muted-foreground p-3 bg-muted rounded-md">
            현재 보고된 트래커 오류 없음
          </div>
        {/if}

        <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <div class="space-y-1.5">
            <Label for="ip">IP 주소 (기본값: 0.0.0.0)</Label>
            <Input
              id="ip"
              type="text"
              bind:value={ipAddress}
              placeholder="0.0.0.0"
            />
            {#if ipAddress.trim() !== '' && !isIpValid(ipAddress)}
              <p class="text-xs text-destructive">
                올바른 IPv4 주소 형식이 아닙니다.
              </p>
            {/if}
          </div>
          <div class="space-y-1.5">
            <Label for="port">포트 (기본값: 46821)</Label>
            <Input
              id="port"
              type="number"
              bind:value={port}
              placeholder="46821"
              min="1"
              max="65535"
            />
            {#if port.trim() !== '' && !isPortValid(port === '' ? undefined : +port)}
              <p class="text-xs text-destructive">
                1 ~ 65535 사이의 숫자를 입력하세요.
              </p>
            {/if}
          </div>
        </div>

        <!-- <div class="flex items-center space-x-2 pt-3">
          <Checkbox id="secure-connection" bind:checked={useSecureConnection} />
          <Label for="secure-connection" class="font-normal cursor-pointer"
            >보안 연결 (HTTPS/WSS) 사용</Label
          >
        </div> -->

        <div class="flex items-center space-x-2 pt-3">
          <Checkbox id="auto-attach" bind:checked={autoAttachEnabled} />
          <Label for="auto-attach" class="font-normal cursor-pointer">
            게임 자동 연결
          </Label>
        </div>

        <Button onclick={applyAndRestart} class="w-full mt-4">
          <IconRestart class="mr-2 h-4 w-4" />
          설정 적용 및 서버 재시작
        </Button>
      </div>
    {/if}
  </div>
</main>
