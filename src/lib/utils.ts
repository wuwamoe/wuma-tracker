import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from '@tauri-apps/plugin-process';
import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";
import toast from "svelte-5-french-toast";

export function cn(...inputs: ClassValue[]) {
	return twMerge(clsx(inputs));
}

export function delay(ms: number) {
	return new Promise( resolve => setTimeout(resolve, ms) );
}

export async function checkUpdates() {
    check().then(async (update) => {
			if (update) {
        const promise = update.downloadAndInstall();
        await toast.promise(promise, {
            loading: `업데이트 ${update.version} 다운로드 중...`,
            success: "업데이트 성공! 5초 후에 재시작됩니다.",
            error: "업데이트 실패. 다시 시도해주세요.",
        });
		await delay(5000);
        await relaunch();
    } else {
			toast.success("최신 버전입니다!");
		}
		}).catch(() => {
			toast.error("업데이트 확인 실패")
		})

}
