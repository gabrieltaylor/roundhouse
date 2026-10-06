module Ledger
  class ArchivedInvoice < ApplicationRecord
    self.table_name = "documents"
    self.primary_key = "code"
  end
end
