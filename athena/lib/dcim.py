import os
import board
import sdcardio
import storage

class DCIM:
    dirname = "ATHENA"
    prefix = "ATH_"

    vfs = None

    def __init__(self, sd):
        self.sd = sd
        self.vfs = storage.VfsFat(sd)
        storage.mount(self.vfs, '/sd')

        self.init_filestructure()

    def init_filestructure(self):
        try:
            print(f"Our directory ('{self.dirname}') already exists")
            self.vfs.stat(self.dirname)
        except Exception as e:
            print(f"Our directory ('{self.dirname}') doesn't seem to exist: {e}")
            self.vfs.mkdir(self.dirname)

    def store(self, data, extension = ".JPG"):
        max_id = 0

        for (filename, mode, zero, filesize) in self.vfs.ilistdir(self.dirname):
            if filename.startswith(self.prefix) and filename.endswith(extension):
                try:
                    photo_id = int(filename.replace(self.prefix, "").replace(extension, ""))
                except Exception as e:
                    print(f"Could not parse name of existing file: '{filename}'")
                    photo_id = 0

                max_id = max(photo_id, max_id)

        next_name = f"{self.prefix}{max_id+1:06}{extension}"
        try:
            f = self.vfs.open(f"{self.dirname}/{next_name}", "w")
            f.write(data)
            f.close()

            return (True, next_name)
        except Exception as e:
            return (False, e)




