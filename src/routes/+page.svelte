<script lang="ts">
  // Tauri API 및 플러그인 임포트
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { exit } from '@tauri-apps/plugin-process';
  import { getVersion } from '@tauri-apps/api/app';

  // Svelte 및 라이브러리 임포트
  import toast from 'svelte-5-french-toast';
  import { writeText } from '@tauri-apps/plugin-clipboard-manager';
  import { open as openFileDialog } from '@tauri-apps/plugin-dialog';

  // 컴포넌트 임포트 (shadcn-svelte 등)
  import { Button, buttonVariants } from '@/components/ui/button';
  import { Label } from '@/components/ui/label';
  // Checkbox 임포트 추가
  import { Badge } from '@/components/ui/badge';
  import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';

  // 아이콘 임포트
  import MaterialSymbolsKeyboardArrowDownRounded from '~icons/material-symbols/keyboard-arrow-down-rounded';
  import MaterialSymbolsKeyboardArrowUpRounded from '~icons/material-symbols/keyboard-arrow-up-rounded';
  import IconCopy from '~icons/material-symbols/content-copy-outline-rounded'; // 복사 아이콘
  // [추가] 외부 연결(공유)에 어울리는 아이콘 임포트
  import IconLan from '~icons/material-symbols/lan-outline-rounded';
  import IconConnection from '~icons/mdi/connection';
  import IconPower from '~icons/mdi/power';
  import IconRestart from '~icons/mdi/restart';
  import IconPlay from '~icons/mdi/play-circle-outline';
  import IconFolder from '~icons/material-symbols/folder-open-outline-rounded';

  // 타입 및 유틸리티 임포트
  import type PlayerInfo from '$lib/types/PlayerInfo';
  import type AppConfig from '@/types/Config';
  import { checkUpdates } from '$lib/utils';
  import type GlobalState from '@/types/GlobalState';
  import { Checkbox } from '@/components/ui/checkbox';
  import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
  import {
    Dialog,
    DialogContent,
    DialogHeader,
    DialogTitle,
    DialogDescription,
    DialogFooter,
  } from '@/components/ui/dialog';
  import {
    Tooltip,
    TooltipContent,
    TooltipProvider,
    TooltipTrigger,
  } from '@/components/ui/tooltip';

  let globalState = $state<GlobalState>({
    procState: 0,
    serverState: 0,
  });
  let pLocation = $state<PlayerInfo>(); // 플레이어 위치 정보
  let settingsExpanded = $state<boolean>(false); // 고급 설정 확장 여부
  let trackerError = $state(''); // 트래커 오류 메시지
  let errorLastUpdated = $state<number | null>(null); // 마지막 오류 수신 시각
  let errorElapsed = $state(0); // 오류 표시 경과 시간(초)
  const showError = $derived(trackerError !== '' && errorElapsed < 60);
  let appversion = $state(''); // 앱 버전
  let autoAttachEnabled = $state(false);
  let connectingExternal = $state(false);
  let startInTray = $state(false);
  let savedGamePath = $state<string | null>(null);

  // 게임 경로 선택 UI 상태
  let showGamePathPicker = $state(false);
  let gameCandidates = $state<string[]>([]);
  let selectedCandidate = $state('');
  let pickerMode = $state<'launch' | 'save'>('launch');
  let launchScanning = $state(false);

  async function silentAttach() {
    try {
      await invoke('find_and_attach');
      trackerError = '';
      errorLastUpdated = null;
      toast.success('자동으로 게임에 연결되었습니다.');
    } catch (err) {
      // console.error('Auto-attach failed:', err);
    }
  }

  async function connectExternal() {
    // toast.promise를 사용하여 비동기 작업의 상태를 사용자에게 보여줍니다.
    const promise = invoke<string>('restart_external_signaling_client'); // 새 Tauri 커맨드 호출

    toast.promise(promise, {
      loading: '외부 공유방 생성 및 연결 중...',
      success: (roomCode) => `연결 성공! 방 코드: ${roomCode}`,
      error: (err) => {
        console.error('External connection failed:', err);
        return `방 생성 실패: ${err}`;
      },
    });
  }

  async function copyCodeToClipboard() {
    if (!globalState.externalConnectionCode) return;
    try {
      await writeText(globalState.externalConnectionCode);
      toast.success('방 코드가 클립보드에 복사되었습니다.');
    } catch (err) {
      console.error('Failed to copy code: ', err);
      toast.error('방 코드 복사에 실패했습니다.');
    }
  }

  // 게임 연결 폴링
  $effect(() => {
    let intervalId: ReturnType<typeof setInterval> | undefined;

    // 자동 연결 기능이 활성화되어 있고, 게임이 연결되지 않았을 때만 인터벌 시작
    if (autoAttachEnabled && globalState.procState === 0) {
      intervalId = setInterval(() => {
        if (globalState.procState === 0) {
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
        autoAttachEnabled = config.autoAttachEnabled ?? false;
        startInTray = config.startInTray ?? false;
        savedGamePath = config.gamePath ?? null;
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
      errorLastUpdated = Date.now();
      errorElapsed = 0;
    });
    const unlistenToastError = listen<string>('report-error-toast', (e) => {
      console.error(e.payload);
      toast.error(e.payload, {
        duration: 3000,
        position: 'top-center',
      });
    });
    const unlistenServerState = listen<GlobalState>(
      'handle-global-state-change',
      (e) => {
        console.log(e.payload);
        globalState = e.payload;
      },
    );

    // 컴포넌트 언마운트 시 리스너 해제
    return () => {
      unlistenLocation.then((f) => f());
      unlistenError.then((f) => f());
      unlistenServerState.then((f) => f());
      unlistenToastError.then((f) => f());
    };
  });

  // 오류 타이머: 오류 수신 시점부터 초 단위로 경과 시간 추적, 60초 후 자동 숨김
  $effect(() => {
    const updated = errorLastUpdated;
    if (!updated) return;

    errorElapsed = 0;
    const interval = setInterval(() => {
      errorElapsed = Math.floor((Date.now() - updated) / 1000);
    }, 1000);

    return () => clearInterval(interval);
  });

  // --- 유효성 검사 함수 ---

  // --- 이벤트 핸들러 ---
  async function attach(event: Event) {
    event.preventDefault();
    toast.promise(invoke('find_and_attach'), {
      loading: '게임 프로세스 찾는 중...',
      success: () => {
        trackerError = '';
        errorLastUpdated = null;
        return '게임에 성공적으로 연결되었습니다.';
      },
      error: (err) => {
        console.error('Attach failed:', err);
        return `게임 연결 실패: ${err instanceof Error ? err.message : String(err)}`;
      },
    });
  }

  async function launchGame() {
    if (savedGamePath) {
      doLaunch(savedGamePath);
      return;
    }
    await openGamePathPicker('launch');
  }

  async function openGamePathPicker(mode: 'launch' | 'save') {
    launchScanning = true;
    try {
      const candidates = await invoke<string[]>('scan_game_candidates');
      gameCandidates = candidates;
      selectedCandidate = candidates[0] ?? '';
      pickerMode = mode;
      if (candidates.length === 0) {
        await pickCustomPath(mode);
      } else {
        showGamePathPicker = true;
      }
    } catch (err) {
      toast.error(`경로 탐색 실패: ${err}`);
    } finally {
      launchScanning = false;
    }
  }

  async function pickCustomPath(mode: 'launch' | 'save') {
    const selected = await openFileDialog({
      title: '게임 실행 파일 선택 (Client-Win64-Shipping.exe)',
      filters: [{ name: 'Client-Win64-Shipping.exe', extensions: ['exe'] }],
    });
    if (!selected) return;
    const path = Array.isArray(selected) ? selected[0] : selected;
    const filename = path.split(/[\\/]/).pop() ?? '';
    if (filename !== 'Client-Win64-Shipping.exe') {
      toast.error('Client-Win64-Shipping.exe 파일을 선택해주세요.');
      return;
    }
    showGamePathPicker = false;
    if (mode === 'launch') {
      doLaunch(path);
    } else {
      await saveGamePath(path);
    }
  }

  async function confirmPickerSelection() {
    showGamePathPicker = false;
    if (pickerMode === 'launch') {
      doLaunch(selectedCandidate);
    } else {
      await saveGamePath(selectedCandidate);
    }
  }

  function doLaunch(path: string) {
    const promise = invoke('launch_and_attach', { path });
    promise
      .then(() => {
        savedGamePath = path;
        trackerError = '';
        errorLastUpdated = null;
      })
      .catch(() => {});
    toast.promise(promise, {
      loading: '게임 실행 중... (최대 60초 소요)',
      success: '게임 실행됨!',
      error: (err) => `게임 실행 실패: ${err instanceof Error ? err.message : String(err)}`,
    });
  }

  async function saveGamePath(path: string) {
    try {
      const config = await invoke<AppConfig>('channel_get_config');
      await invoke('write_config', {
        ip: config.ip ?? null,
        port: config.port ?? null,
        useSecureConnection: config.useSecureConnection ?? null,
        autoAttachEnabled,
        startInTray,
        gamePath: path,
      });
      savedGamePath = path;
      toast.success('게임 경로가 저장되었습니다.');
    } catch (err) {
      toast.error(`경로 저장 실패: ${err}`);
    }
  }

  async function applyAndRestart(event: Event) {
    event.preventDefault();

    const handler = async () => {
      await invoke('write_config', {
        ip: null,
        port: null,
        useSecureConnection: null,
        autoAttachEnabled: autoAttachEnabled,
        startInTray: startInTray,
        gamePath: savedGamePath,
      });
      await invoke('restart_server');
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

  let formattedConnectionCode = $derived(
    globalState.externalConnectionCode
      ? `${globalState.externalConnectionCode.substring(0, 4)}-${globalState.externalConnectionCode.substring(4)}`
      : '',
  );
</script>

<main class="container max-w-xl mx-auto py-6 px-4 space-y-6">
  <TooltipProvider>
    <div class="space-y-4 p-4 border rounded-lg bg-background shadow-sm">
      <div class="flex items-center justify-between pb-2 border-b">
        <h1 class="text-xl font-semibold">명조 맵스 트래커</h1>
        <div class="flex items-center space-x-2">
          <Tooltip>
            <TooltipTrigger
              class={buttonVariants({
                variant: 'ghost',
                size: 'icon',
                class: 'rounded-full',
              })}
              onclick={connectExternal}
              disabled={connectingExternal}
              aria-label="외부 공유방 연결"
            >
              <IconLan class="h-5 w-5" />
            </TooltipTrigger>

            <TooltipContent>
              <p>외부 공유방 생성/연결</p>
            </TooltipContent>
          </Tooltip>

          {#if appversion}
            <Badge variant="outline">v{appversion}</Badge>
          {/if}
        </div>
      </div>

      <div class="grid grid-cols-2 gap-2">
        <div
          class={`flex items-center gap-2 rounded-md px-3 py-2 text-sm font-medium ${globalState.procState === 0 ? 'bg-red-500/10 text-red-500' : 'bg-green-500/10 text-green-600 dark:text-green-400'}`}
        >
          <div
            class={`w-2 h-2 rounded-full flex-shrink-0 ${globalState.procState === 0 ? 'bg-red-500' : 'bg-green-500'}`}
          ></div>
          {globalState.procState === 0 ? '게임 미연결' : '게임 연결됨'}
          {#if globalState.procState === 1 && globalState.activeOffsetName}
            <Badge variant="secondary" class="ml-auto text-xs"
              >{globalState.activeOffsetName}</Badge
            >
          {/if}
          {#if autoAttachEnabled && globalState.procState === 0}
            <IconRestart class="ml-auto h-3.5 w-3.5 animate-reverse-spin" />
          {/if}
        </div>

        <div
          class={`flex items-center gap-2 rounded-md px-3 py-2 text-sm font-medium ${globalState.serverState === 0 ? 'bg-red-500/10 text-red-500' : 'bg-green-500/10 text-green-600 dark:text-green-400'}`}
        >
          <div
            class={`w-2 h-2 rounded-full flex-shrink-0 ${globalState.serverState === 0 ? 'bg-red-500' : 'bg-green-500'}`}
          ></div>
          {globalState.serverState === 0 ? '서버 비활성' : '서버 활성'}
        </div>
      </div>

      {#if globalState.externalConnectionCode}
        <div
          class="flex items-center gap-2 rounded-md bg-muted px-3 py-2 text-sm"
        >
          <IconConnection class="h-4 w-4 text-muted-foreground shrink-0" />
          <span class="text-muted-foreground">공유방 코드</span>
          <Badge
            variant="secondary"
            class="ml-auto font-mono text-base cursor-pointer hover:bg-primary/20"
            onclick={copyCodeToClipboard}
            title="클릭하여 복사"
          >
            {formattedConnectionCode}
            <IconCopy class="ml-1.5 h-3.5 w-3.5" />
          </Badge>
        </div>
      {/if}

      {#if globalState.procState === 1 && pLocation}
        <div class="grid grid-cols-3 gap-2 text-sm text-center">
          <div class="bg-muted rounded-md py-2">
            <p class="text-xs text-muted-foreground mb-0.5">X</p>
            <p class="font-mono font-medium">{Math.round(pLocation.x / 100)}</p>
          </div>
          <div class="bg-muted rounded-md py-2">
            <p class="text-xs text-muted-foreground mb-0.5">Y</p>
            <p class="font-mono font-medium">{Math.round(pLocation.y / 100)}</p>
          </div>
          <div class="bg-muted rounded-md py-2">
            <p class="text-xs text-muted-foreground mb-0.5">Z</p>
            <p class="font-mono font-medium">{Math.round(pLocation.z / 100)}</p>
          </div>
        </div>
      {/if}

      <div class="flex gap-2 pt-2">
        {#if globalState.procState === 0}
          <Button class="flex-1" onclick={launchGame} disabled={launchScanning}>
            <IconPlay class="mr-2 h-4 w-4" />
            {launchScanning ? '경로 탐색 중...' : '게임 실행'}
          </Button>
        {/if}
        <Button
          class="flex-1"
          variant={globalState.procState === 1 ? 'outline' : 'default'}
          onclick={attach}
        >
          <IconConnection class="mr-2 h-4 w-4" />
          {globalState.procState === 1 ? '재연결' : '게임 연결'}
        </Button>
        <Button variant="destructive" class="flex-1" onclick={quit}>
          <IconPower class="mr-2 h-4 w-4" />
          종료
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
          {#if showError}
            <Alert variant="destructive">
              <AlertTitle>트래커 오류 ({errorElapsed}초)</AlertTitle>
              <AlertDescription>{trackerError}</AlertDescription>
            </Alert>
          {:else}
            <div class="text-sm text-muted-foreground p-3 bg-muted rounded-md">
              현재 보고된 트래커 오류 없음
            </div>
          {/if}

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

          <div class="flex items-center space-x-2 pt-3">
            <Checkbox id="start-in-tray" bind:checked={startInTray} />
            <Label for="start-in-tray" class="font-normal cursor-pointer">
              트레이 상태로 시작 (윈도우 숨김)
            </Label>
          </div>

          <div class="space-y-2 pt-3">
            <Label class="text-sm font-medium">게임 경로</Label>
            {#if savedGamePath}
              <p class="text-xs font-mono text-muted-foreground break-all">{savedGamePath}</p>
            {:else}
              <p class="text-xs text-muted-foreground">저장된 경로 없음</p>
            {/if}
            <Button
              variant="outline"
              size="sm"
              class="w-full"
              onclick={() => openGamePathPicker('save')}
              disabled={launchScanning}
            >
              <IconFolder class="mr-2 h-4 w-4" />
              경로 {savedGamePath ? '변경' : '설정'}
            </Button>
          </div>

          <Button onclick={applyAndRestart} class="w-full mt-4">
            <IconRestart class="mr-2 h-4 w-4" />
            설정 적용 및 서버 재시작
          </Button>
        </div>
      {/if}
    </div>
  </TooltipProvider>
</main>

<Dialog bind:open={showGamePathPicker}>
  <DialogContent class="max-w-md">
    <DialogHeader>
      <DialogTitle>게임 경로 선택</DialogTitle>
      <DialogDescription>
        실행할 게임 경로를 선택하거나 직접 지정하세요.
      </DialogDescription>
    </DialogHeader>

    <div class="py-2 space-y-2">
      {#if gameCandidates.length > 0}
        <RadioGroup bind:value={selectedCandidate} class="gap-2">
          {#each gameCandidates as candidate}
            <label
              for={candidate}
              class="flex items-center gap-3 rounded-md border px-3 py-2.5 cursor-pointer transition-colors hover:bg-accent {selectedCandidate === candidate ? 'border-primary bg-primary/5' : 'border-border'}"
            >
              <RadioGroupItem value={candidate} id={candidate} />
              <span class="font-mono text-xs break-all">{candidate}</span>
            </label>
          {/each}
        </RadioGroup>
      {:else}
        <p class="text-sm text-muted-foreground py-1">자동으로 탐지된 경로가 없습니다.</p>
      {/if}

      <div class="relative py-2">
        <div class="absolute inset-0 flex items-center"><span class="w-full border-t"></span></div>
        <div class="relative flex justify-center">
          <span class="bg-background px-2 text-xs text-muted-foreground">또는</span>
        </div>
      </div>

      <Button variant="outline" class="w-full" onclick={() => pickCustomPath(pickerMode)}>
        <IconFolder class="mr-2 h-4 w-4" />
        직접 선택...
      </Button>
    </div>

    <DialogFooter class="gap-2">
      <Button variant="outline" onclick={() => (showGamePathPicker = false)}>취소</Button>
      <Button disabled={!selectedCandidate} onclick={confirmPickerSelection}>
        {pickerMode === 'launch' ? '선택 후 실행' : '저장'}
      </Button>
    </DialogFooter>
  </DialogContent>
</Dialog>
