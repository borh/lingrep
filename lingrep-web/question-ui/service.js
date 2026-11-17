export const serviceURL = (path, base = document.baseURI) => new URL(path.replace(/^\/+/, ''), base).href;
export class ServiceError extends Error {
    status;
    constructor(status, message) {
        super(message);
        this.status = status;
    }
}
export const expiredSearch = (error) => error instanceof ServiceError && (error.status === 404 || error.status === 410);
/** Builds a Workbench permalink request with compound derivation disabled. */
export const workbenchLinkRequest = (source, corpus) => new URLSearchParams({ pattern: source, corpus, derive_compounds: 'false' }).toString();
/** Returns null unless `payload.path` starts with `?`. */
export const workbenchHref = (payload, base = document.baseURI) => {
    const path = payload?.path;
    return typeof path === 'string' && path.startsWith('?') ? serviceURL('workbench' + path, base) : null;
};
